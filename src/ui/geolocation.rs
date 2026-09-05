//! One-shot system location; invoked only by the explicit location button.
use crate::tg::GeoPoint;
use gio::prelude::*;
use gtk4::{gio, glib};
use std::time::Duration;

const BUS: &str = "org.freedesktop.GeoClue2";
const CLIENT: &str = "org.freedesktop.GeoClue2.Client";

struct ClientLease {
    client: gio::DBusProxy,
    manager: gio::DBusProxy,
    path: glib::variant::ObjectPath,
}
impl Drop for ClientLease {
    fn drop(&mut self) {
        // Also runs if the location dialog cancels the waiting future.
        self.client.call(
            "Stop",
            None,
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            |_| {},
        );
        self.manager.call(
            "DeleteClient",
            Some(&(self.path.clone(),).to_variant()),
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}
async fn proxy(path: &str, interface: &str) -> Result<gio::DBusProxy, glib::Error> {
    gio::DBusProxy::for_bus_future(
        gio::BusType::System,
        gio::DBusProxyFlags::NONE,
        None,
        BUS,
        path,
        interface,
    )
    .await
}
async fn locate() -> Result<(GeoPoint, f64), String> {
    let manager = proxy(
        "/org/freedesktop/GeoClue2/Manager",
        "org.freedesktop.GeoClue2.Manager",
    )
    .await
    .map_err(|_| "System location is unavailable. Choose a point or enter coordinates.")?;
    let path = manager
        .call_future("GetClient", None, gio::DBusCallFlags::NONE, 5000)
        .await
        .map_err(|_| "System location is unavailable")?
        .get::<(glib::variant::ObjectPath,)>()
        .ok_or("Invalid location service response")?
        .0;
    let client = proxy(path.as_str(), CLIENT)
        .await
        .map_err(|_| "System location is unavailable")?;
    let lease = ClientLease {
        client,
        manager,
        path,
    };
    for (name, value) in [
        ("DesktopId", "omarchygram".to_variant()),
        ("RequestedAccuracyLevel", 8u32.to_variant()),
    ] {
        lease
            .client
            .call_future(
                "org.freedesktop.DBus.Properties.Set",
                Some(&(CLIENT, name, value).to_variant()),
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await
            .map_err(
                |_| "System location access was not granted. Use a map point or coordinates.",
            )?;
    }
    let (sender, receiver) = async_channel::bounded(1);
    lease
        .client
        .connect_local("g-signal", false, move |values| {
            if values[2].get::<String>().ok().as_deref() == Some("LocationUpdated")
                && let Ok(parameters) = values[3].get::<glib::Variant>()
                && let Some((_, new)) =
                    parameters.get::<(glib::variant::ObjectPath, glib::variant::ObjectPath)>()
            {
                let _ = sender.try_send(new);
            }
            None
        });
    lease
        .client
        .call_future("Start", None, gio::DBusCallFlags::NONE, 15000)
        .await
        .map_err(|_| "System location access was not granted. Use a map point or coordinates.")?;
    let location = receiver
        .recv()
        .await
        .map_err(|_| "Location service disconnected")?;
    let location = proxy(location.as_str(), "org.freedesktop.GeoClue2.Location")
        .await
        .map_err(|_| "Location is unavailable")?;
    let number = |name| {
        location
            .cached_property(name)
            .and_then(|value| value.get::<f64>())
            .ok_or("Location is unavailable")
    };
    let point = GeoPoint {
        lat: number("Latitude")?,
        lon: number("Longitude")?,
    };
    if !point.lat.is_finite()
        || !point.lon.is_finite()
        || !(-90.0..=90.0).contains(&point.lat)
        || !(-180.0..=180.0).contains(&point.lon)
    {
        return Err("Invalid system location".into());
    }
    Ok((point, number("Accuracy").unwrap_or(f64::NAN)))
}
pub async fn current() -> Result<(GeoPoint, f64), String> {
    glib::future_with_timeout(Duration::from_secs(30), locate())
        .await
        .map_err(|_| "Location timed out. Choose a map point or enter coordinates.")?
}
