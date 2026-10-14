//! zbus on the host thread. Needs a session bus; skipped without one.

#![cfg(feature = "dbus")]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use futures_lite::{StreamExt, future};
use tessela_service::dbus::zbus;
use tessela_service::{Host, Timer, dbus};

#[test]
fn shared_session_connection_and_signals_from_a_name() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("no session bus, skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tessela-service-dbus-{}", std::process::id()));
    let host = Host::new().dir(dir).keep_running_without_parent();
    let stop = host.shutdown_handle();
    let ok = Rc::new(Cell::new(false));
    let done = ok.clone();
    host.run(async move {
        let conn = dbus::session().await.unwrap();
        // The same connection for everyone.
        assert_eq!(
            dbus::session().await.unwrap().unique_name(),
            conn.unique_name()
        );
        let proxy = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
        assert!(!proxy.get_id().await.unwrap().to_string().is_empty());

        // A name appearing counts as a change; its owner's signals too.
        let name: &'static str =
            Box::leak(format!("org.tessela.ServiceTest.p{}", std::process::id()).into_boxed_str());
        let mut changes = dbus::signals_from(&conn, name).await.unwrap();
        let other = dbus::connect(zbus::connection::Builder::session().unwrap())
            .await
            .unwrap();
        other.request_name(name).await.unwrap();
        let got = future::or(async { changes.next().await.is_some() }, async {
            Timer::after(Duration::from_secs(5)).await;
            false
        })
        .await;
        assert!(got, "owner change seen");
        done.set(true);
        stop.stop();
    })
    .unwrap();
    assert!(ok.get());
}
