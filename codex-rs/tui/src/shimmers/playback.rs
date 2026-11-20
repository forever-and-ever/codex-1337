#[cfg(target_os = "linux")]
pub(super) async fn playing() -> bool {
    use std::future::ready;
    use std::time::Duration;

    use futures::StreamExt as _;
    use futures::stream::FuturesUnordered;
    use zbus::Connection;
    use zbus::Proxy;
    use zbus::fdo::DBusProxy;
    use zbus::proxy::Builder;
    use zbus::proxy::CacheProperties;

    // MPRIS reports media playback, unlike a running sink, which could be playing a notification.
    tokio::time::timeout(Duration::from_millis(/*millis*/ 250), async {
        let bus = Connection::session().await.ok()?;
        let names = DBusProxy::new(&bus).await.ok()?.list_names().await.ok()?;
        let names = names
            .into_iter()
            .filter(|name| name.as_str().starts_with("org.mpris.MediaPlayer2."));
        let players: FuturesUnordered<_> = names
            .map(|name| {
                let bus = &bus;
                async move {
                    let player: Proxy<'_> = Builder::new(bus)
                        .destination(name)
                        .ok()?
                        .path("/org/mpris/MediaPlayer2")
                        .ok()?
                        .interface("org.mpris.MediaPlayer2.Player")
                        .ok()?
                        .cache_properties(CacheProperties::No)
                        .build()
                        .await
                        .ok()?;
                    player.get_property::<String>("PlaybackStatus").await.ok()
                }
            })
            .collect();
        Some(
            players
                .any(|status| ready(status.as_deref() == Some("Playing")))
                .await,
        )
    })
    .await
    .is_ok_and(|playing| playing == Some(true))
}

#[cfg(not(target_os = "linux"))]
pub(super) async fn playing() -> bool {
    false
}
