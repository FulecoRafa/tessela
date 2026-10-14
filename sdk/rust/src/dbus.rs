//! D-Bus through [zbus](https://docs.rs/zbus), on the same runtime.
//!
//! Most desktop data has change signals on the bus (NetworkManager, BlueZ,
//! power-profiles-daemon, MPRIS, logind): subscribing to them instead of
//! polling a CLI is what keeps a service idle. Every service in the process
//! shares one connection per bus.
//!
//! zbus is built with `async-io` (not `tokio`), the runtime this crate uses;
//! it runs its socket I/O on its own small thread.

use async_lock::OnceCell;
use futures_lite::{Stream, StreamExt};
use zbus::names::OwnedUniqueName;
use zbus::{Connection, MatchRule, MessageStream};

pub use zbus;

static SESSION: OnceCell<Connection> = OnceCell::new();
static SYSTEM: OnceCell<Connection> = OnceCell::new();

/// The process's session bus connection.
pub async fn session() -> zbus::Result<Connection> {
    SESSION.get_or_try_init(Connection::session).await.cloned()
}

/// The process's system bus connection.
pub async fn system() -> zbus::Result<Connection> {
    SYSTEM.get_or_try_init(Connection::system).await.cloned()
}

/// Messages matching `rule` (the match is registered on the bus and removed
/// when the stream is dropped).
pub async fn watch(conn: &Connection, rule: MatchRule<'static>) -> zbus::Result<MessageStream> {
    MessageStream::for_match_rule(rule, conn, None).await
}

/// One item whenever the owner of `name` sends a signal, appears or goes
/// away: the "something changed, read again" stream for services whose
/// signals are easier to answer with a fresh read. Pair it with a short
/// debounce.
///
/// Only the current owner's signals count: zbus cannot match a well-known
/// sender on the client side, and a shared connection also receives the
/// signals other streams asked for.
pub async fn signals_from(
    conn: &Connection,
    name: &'static str,
) -> zbus::Result<impl Stream<Item = ()> + use<>> {
    enum Ev {
        Signal(Option<OwnedUniqueName>),
        Owner(Option<OwnedUniqueName>),
    }
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let wk = zbus::names::BusName::try_from(name)?;
    let mut owner: Option<OwnedUniqueName> = dbus.get_name_owner(wk).await.ok();
    let owners = dbus
        .receive_name_owner_changed_with_args(&[(0, name)])
        .await?
        .map(|s| {
            Ev::Owner(
                s.args()
                    .ok()
                    .and_then(|a| a.new_owner().as_ref().map(|o| o.to_owned().into())),
            )
        });
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(name)?
        .build();
    let signals = watch(conn, rule).await?.map(|m| {
        Ev::Signal(
            m.ok()
                .and_then(|m| m.header().sender().map(|s| s.to_owned().into())),
        )
    });
    Ok(owners.or(signals).filter_map(move |ev| match ev {
        Ev::Owner(new) => {
            owner = new;
            Some(())
        }
        Ev::Signal(sender) => (sender.is_some() && sender == owner).then_some(()),
    }))
}
