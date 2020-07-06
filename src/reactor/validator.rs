//! Reactor for validator nodes.
//!
//! Validator nodes join the validator-only network upon startup.

mod config;
mod error;

use std::fmt::{self, Display, Formatter};

use derive_more::From;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use tracing::Span;

use crate::{
    components::{
        api_server::{self, ApiServer},
        consensus::{self, EraSupervisor},
        deploy_gossiper::{self, DeployGossiper},
        pinger::{self, Pinger},
        storage::{Storage, StorageType},
        Component,
    },
    effect::{
        announcements::NetworkAnnouncement,
        requests::{ApiRequest, DeployGossiperRequest, NetworkRequest, StorageRequest},
        Effect, EffectBuilder, Multiple,
    },
    reactor::{self, EventQueueHandle},
    small_network::{self, NodeId},
    SmallNetwork,
};
pub use config::Config;
use error::Error;

/// Reactor message.
#[derive(Debug, Clone, From, Serialize, Deserialize)]
pub enum Message {
    /// Pinger component message.
    #[from]
    Pinger(pinger::Message),
    /// Consensus component message.
    #[from]
    Consensus(consensus::ConsensusMessage),
    /// Deploy gossiper component message.
    #[from]
    DeployGossiper(deploy_gossiper::Message),
}

impl Display for Message {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Message::Pinger(pinger) => write!(f, "Pinger::{}", pinger),
            Message::Consensus(consensus) => write!(f, "Consensus::{}", consensus),
            Message::DeployGossiper(deploy) => write!(f, "DeployGossiper::{}", deploy),
        }
    }
}

/// Top-level event for the reactor.
#[derive(Debug, From)]
#[must_use]
pub enum Event {
    /// Network event.
    #[from]
    Network(small_network::Event<Message>),
    /// Pinger event.
    #[from]
    Pinger(pinger::Event),
    #[from]
    /// Storage event.
    Storage(StorageRequest<Storage>),
    #[from]
    /// API server event.
    ApiServer(api_server::Event),
    #[from]
    /// Consensus event.
    Consensus(consensus::Event),
    /// Deploy gossiper event.
    #[from]
    DeployGossiper(deploy_gossiper::Event),

    // Requests
    /// Network request.
    #[from]
    NetworkRequest(NetworkRequest<NodeId, Message>),

    // Announcements
    /// Network announcement.
    #[from]
    NetworkAnnouncement(NetworkAnnouncement<NodeId, Message>),
}

impl From<ApiRequest> for Event {
    fn from(request: ApiRequest) -> Self {
        Event::ApiServer(api_server::Event::ApiRequest(request))
    }
}

impl From<NetworkRequest<NodeId, consensus::ConsensusMessage>> for Event {
    fn from(request: NetworkRequest<NodeId, consensus::ConsensusMessage>) -> Self {
        Event::NetworkRequest(request.map_payload(Message::from))
    }
}

impl From<NetworkRequest<NodeId, pinger::Message>> for Event {
    fn from(request: NetworkRequest<NodeId, pinger::Message>) -> Self {
        Event::NetworkRequest(request.map_payload(Message::from))
    }
}

impl From<NetworkRequest<NodeId, deploy_gossiper::Message>> for Event {
    fn from(request: NetworkRequest<NodeId, deploy_gossiper::Message>) -> Self {
        Event::NetworkRequest(request.map_payload(Message::from))
    }
}

impl From<DeployGossiperRequest> for Event {
    fn from(request: DeployGossiperRequest) -> Self {
        Event::DeployGossiper(deploy_gossiper::Event::Request(request))
    }
}

/// Validator node reactor.
#[derive(Debug)]
pub struct Reactor {
    net: SmallNetwork<Event, Message>,
    pinger: Pinger,
    storage: Storage,
    api_server: ApiServer,
    consensus: EraSupervisor,
    deploy_gossiper: DeployGossiper,
    rng: ChaCha20Rng,
}

impl reactor::Reactor for Reactor {
    type Event = Event;
    type Config = Config;
    type Error = Error;

    fn new(
        cfg: Self::Config,
        event_queue: EventQueueHandle<Self::Event>,
        span: &Span,
    ) -> Result<(Self, Multiple<Effect<Event>>), Error> {
        let effect_builder = EffectBuilder::new(event_queue);
        let (net, net_effects) = SmallNetwork::new(event_queue, cfg.validator_net)?;
        span.record("id", &tracing::field::display(net.node_id()));

        let (pinger, pinger_effects) = Pinger::new(effect_builder);
        let storage = Storage::new(cfg.storage)?;
        let (api_server, api_server_effects) = ApiServer::new(cfg.http_server, effect_builder);
        let consensus = EraSupervisor::new();
        let deploy_gossiper = DeployGossiper::new(cfg.gossip);

        let mut effects = reactor::wrap_effects(Event::Network, net_effects);
        effects.extend(reactor::wrap_effects(Event::Pinger, pinger_effects));
        effects.extend(reactor::wrap_effects(Event::ApiServer, api_server_effects));

        let rng = ChaCha20Rng::from_entropy();

        Ok((
            Reactor {
                net,
                pinger,
                storage,
                api_server,
                consensus,
                deploy_gossiper,
                rng,
            },
            effects,
        ))
    }

    fn dispatch_event(
        &mut self,
        effect_builder: EffectBuilder<Self::Event>,
        event: Event,
    ) -> Multiple<Effect<Self::Event>> {
        match event {
            Event::Network(event) => reactor::wrap_effects(
                Event::Network,
                self.net.handle_event(effect_builder, &mut self.rng, event),
            ),
            Event::Pinger(event) => reactor::wrap_effects(
                Event::Pinger,
                self.pinger
                    .handle_event(effect_builder, &mut self.rng, event),
            ),
            Event::Storage(event) => reactor::wrap_effects(
                Event::Storage,
                self.storage
                    .handle_event(effect_builder, &mut self.rng, event),
            ),
            Event::ApiServer(event) => reactor::wrap_effects(
                Event::ApiServer,
                self.api_server
                    .handle_event(effect_builder, &mut self.rng, event),
            ),
            Event::Consensus(event) => reactor::wrap_effects(
                Event::Consensus,
                self.consensus
                    .handle_event(effect_builder, &mut self.rng, event),
            ),
            Event::DeployGossiper(event) => reactor::wrap_effects(
                Event::DeployGossiper,
                self.deploy_gossiper
                    .handle_event(effect_builder, &mut self.rng, event),
            ),

            // Requests:
            Event::NetworkRequest(req) => self.dispatch_event(
                effect_builder,
                Event::Network(small_network::Event::from(req)),
            ),

            // Announcements:
            Event::NetworkAnnouncement(NetworkAnnouncement::MessageReceived {
                sender,
                payload,
            }) => {
                let reactor_event = match payload {
                    Message::Consensus(msg) => {
                        Event::Consensus(consensus::Event::MessageReceived { sender, msg })
                    }
                    Message::Pinger(msg) => {
                        Event::Pinger(pinger::Event::MessageReceived { sender, msg })
                    }
                    Message::DeployGossiper(message) => {
                        Event::DeployGossiper(deploy_gossiper::Event::MessageReceived {
                            sender,
                            message,
                        })
                    }
                };

                // Any incoming message is one for the pinger.
                self.dispatch_event(effect_builder, reactor_event)
            }
        }
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Event::Network(event) => write!(f, "network: {}", event),
            Event::Pinger(event) => write!(f, "pinger: {}", event),
            Event::Storage(event) => write!(f, "storage: {}", event),
            Event::ApiServer(event) => write!(f, "api server: {}", event),
            Event::Consensus(event) => write!(f, "consensus: {}", event),
            Event::DeployGossiper(event) => write!(f, "deploy gossiper: {}", event),
            Event::NetworkRequest(req) => write!(f, "network request: {}", req),
            Event::NetworkAnnouncement(ann) => write!(f, "network announcement: {}", ann),
        }
    }
}
