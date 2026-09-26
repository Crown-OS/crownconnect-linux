//! Calls on a paired phone, placed and answered from this computer.
//!
//! The phone's audio gateway is reached over Bluetooth hands-free, whose audio PipeWire's bluez5
//! backend routes; its `org.pipewire.Telephony` service exposes the gateway and its calls. The
//! service only exists while WirePlumber runs with the bluez5 telephony backend enabled; without
//! it [`watch`] reports [`TelephonyError::Unavailable`] and calls stay control-only over llts.

mod model;

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{MatchRule, MessageStream};

use crate::ipc::proto::CallInfo;
use crate::ipc::server::Responder;
use crate::util::error_chain::error_chain;
use crate::util::unix_time::unix_millis_now;
use model::{Properties, TelephonyModel, CALL_INTERFACE};

const SERVICE: &str = "org.pipewire.Telephony";
const ROOT: &str = "/org/pipewire/Telephony";
const SIGNAL_QUEUE_DEPTH: usize = 32;

/// The Bluetooth service a phone's hands-free audio gateway advertises.
pub const PHONE_AUDIO_GATEWAY_UUID: bluer::Uuid =
    bluer::Uuid::from_u128(0x0000_111f_0000_1000_8000_0080_5f9b_34fb);

#[derive(Debug, thiserror::Error)]
pub enum TelephonyError {
    #[error("session bus: {0}")]
    Bus(#[from] zbus::Error),
    #[error("PipeWire telephony: {0}")]
    Telephony(#[from] zbus::fdo::Error),
    #[error("PipeWire's telephony service is not running")]
    Unavailable,
    #[error("bluetooth: {0}")]
    Bluetooth(#[from] bluer::Error),
    #[error("no phone with address {0} is connected for calls")]
    UnknownPhone(String),
}

/// A phone's calls after something about them changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCalls {
    /// The phone's Bluetooth address.
    pub address: String,
    pub calls: Vec<CallInfo>,
}

/// Asks BlueZ to bring up hands-free with the phone at `address`, so its calls appear in
/// PipeWire's telephony service.
///
/// # Errors
///
/// Fails when BlueZ is unreachable or the phone refuses the profile.
pub async fn connect_hands_free(address: bluer::Address) -> Result<(), TelephonyError> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter
        .device(address)?
        .connect_profile(&PHONE_AUDIO_GATEWAY_UUID)
        .await?;
    Ok(())
}

/// Sends every phone's calls whenever they change, until `events` closes.
///
/// # Errors
///
/// Fails with [`TelephonyError::Unavailable`] when PipeWire's telephony service is absent.
pub async fn watch(events: mpsc::Sender<PhoneCalls>) -> Result<(), TelephonyError> {
    let connection = zbus::Connection::session().await?;
    let manager = object_manager(&connection).await?;
    let objects = manager
        .get_managed_objects()
        .await
        .map_err(|_| TelephonyError::Unavailable)?;
    let mut added = manager.receive_interfaces_added().await?;
    let mut removed = manager.receive_interfaces_removed().await?;
    let mut changed = properties_changed(&connection).await?;
    let mut model = TelephonyModel::default();
    let now = unix_millis_now();
    let mut dirty: Vec<String> = objects
        .into_iter()
        .filter_map(|(path, interfaces)| {
            let interfaces = interfaces
                .into_iter()
                .map(|(name, properties)| (name.to_string(), properties))
                .collect();
            model.add_object(path.as_str(), &interfaces, now)
        })
        .collect();
    loop {
        dirty.sort_unstable();
        dirty.dedup();
        for gateway in dirty.drain(..) {
            let update = PhoneCalls {
                address: model.address(&gateway).unwrap_or_default().to_owned(),
                calls: model.calls_of(&gateway),
            };
            if events.send(update).await.is_err() {
                return Ok(());
            }
        }
        let now = unix_millis_now();
        let touched = tokio::select! {
            Some(signal) = added.next() => signal.args().ok().and_then(|args| {
                let interfaces = args
                    .interfaces_and_properties()
                    .iter()
                    .map(|(name, properties)| (name.to_string(), owned_properties(properties)))
                    .collect();
                model.add_object(args.object_path().as_str(), &interfaces, now)
            }),
            Some(signal) = removed.next() => signal
                .args()
                .ok()
                .and_then(|args| model.remove_object(args.object_path().as_str())),
            Some(Ok(message)) = changed.next() => changed_call(&message)
                .and_then(|(path, properties)| model.update_call(&path, &properties, now)),
            else => return Ok(()),
        };
        dirty.extend(touched);
    }
}

async fn object_manager(connection: &zbus::Connection) -> zbus::Result<ObjectManagerProxy<'_>> {
    ObjectManagerProxy::builder(connection)
        .destination(SERVICE)?
        .path(ROOT)?
        .build()
        .await
}

async fn properties_changed(connection: &zbus::Connection) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SERVICE)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path_namespace(ROOT)?
        .build();
    MessageStream::for_match_rule(rule, connection, Some(SIGNAL_QUEUE_DEPTH)).await
}

fn owned_properties(properties: &HashMap<&str, zbus::zvariant::Value<'_>>) -> Properties {
    properties
        .iter()
        .filter_map(|(key, value)| {
            let owned = OwnedValue::try_from(value.try_clone().ok()?).ok()?;
            Some(((*key).to_owned(), owned))
        })
        .collect()
}

fn changed_call(message: &zbus::Message) -> Option<(String, Properties)> {
    let path = message.header().path()?.to_string();
    let (interface, changed, _): (String, Properties, Vec<String>) =
        message.body().deserialize().ok()?;
    (interface == CALL_INTERFACE).then_some((path, changed))
}

/// Something to do with a phone's calls over hands-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelephonyAction {
    Answer {
        call_id: String,
    },
    /// Declines a ringing call or ends one in progress.
    Hangup {
        call_id: String,
    },
    Dial {
        address: String,
        number: String,
    },
}

/// A [`TelephonyAction`] and the IPC request waiting for its outcome, if a local client asked;
/// a paired device's request has nobody to answer and only logs a failure.
#[derive(Debug)]
pub struct TelephonyJob {
    pub action: TelephonyAction,
    pub reply: Option<Responder<()>>,
}

/// Carries out every job until `jobs` closes.
///
/// # Errors
///
/// Fails without a session bus.
pub async fn serve(mut jobs: mpsc::Receiver<TelephonyJob>) -> Result<(), TelephonyError> {
    let control = TelephonyControl::connect().await?;
    while let Some(job) = jobs.recv().await {
        let outcome = match &job.action {
            TelephonyAction::Answer { call_id } => control.answer(call_id).await,
            TelephonyAction::Hangup { call_id } => control.hangup(call_id).await,
            TelephonyAction::Dial { address, number } => control.dial(address, number).await,
        };
        match (outcome, job.reply) {
            (Ok(()), Some(reply)) => reply.reply(()),
            (Ok(()), None) => {}
            (Err(error), Some(reply)) => reply.fail(error_chain(&error)),
            (Err(error), None) => {
                tracing::warn!(error = %error_chain(&error), "a paired device's call request failed");
            }
        }
    }
    Ok(())
}

/// Answers, ends and places calls through PipeWire's telephony service.
#[derive(Debug, Clone)]
pub struct TelephonyControl {
    connection: zbus::Connection,
}

#[zbus::proxy(
    interface = "org.pipewire.Telephony.Call1",
    default_service = "org.pipewire.Telephony"
)]
trait Call {
    fn answer(&self) -> zbus::Result<()>;
    fn hangup(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.pipewire.Telephony.AudioGateway1",
    default_service = "org.pipewire.Telephony"
)]
trait AudioGateway {
    fn dial(&self, number: &str) -> zbus::Result<()>;
}

impl TelephonyControl {
    /// # Errors
    ///
    /// Fails without a session bus.
    pub async fn connect() -> Result<Self, TelephonyError> {
        Ok(Self {
            connection: zbus::Connection::session().await?,
        })
    }

    /// `call_id` is the [`CallInfo::call_id`] [`watch`] reported.
    ///
    /// # Errors
    ///
    /// Fails when the call is gone or the phone refuses.
    pub async fn answer(&self, call_id: &str) -> Result<(), TelephonyError> {
        self.call(call_id).await?.answer().await?;
        Ok(())
    }

    /// Declining a ringing call and ending an active one are both a hangup.
    ///
    /// # Errors
    ///
    /// Fails when the call is gone or the phone refuses.
    pub async fn hangup(&self, call_id: &str) -> Result<(), TelephonyError> {
        self.call(call_id).await?.hangup().await?;
        Ok(())
    }

    /// # Errors
    ///
    /// Fails when no phone with `address` is connected or it refuses the number.
    pub async fn dial(&self, address: &str, number: &str) -> Result<(), TelephonyError> {
        let objects = object_manager(&self.connection)
            .await?
            .get_managed_objects()
            .await
            .map_err(|_| TelephonyError::Unavailable)?;
        let mut model = TelephonyModel::default();
        for (path, interfaces) in objects {
            let interfaces = interfaces
                .into_iter()
                .map(|(name, properties)| (name.to_string(), properties))
                .collect();
            model.add_object(path.as_str(), &interfaces, 0);
        }
        let gateway = model
            .gateway_by_address(address)
            .ok_or_else(|| TelephonyError::UnknownPhone(address.to_owned()))?;
        AudioGatewayProxy::builder(&self.connection)
            .path(OwnedObjectPath::try_from(gateway).map_err(zbus::Error::from)?)?
            .build()
            .await?
            .dial(number)
            .await?;
        Ok(())
    }

    async fn call(&self, call_id: &str) -> Result<CallProxy<'static>, TelephonyError> {
        Ok(CallProxy::builder(&self.connection)
            .path(OwnedObjectPath::try_from(call_id).map_err(zbus::Error::from)?)?
            .build()
            .await?)
    }
}
