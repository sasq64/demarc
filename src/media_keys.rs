use std::sync::mpsc;

/// Media key events that can be listened to
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKeyEvent {
    Next,
    Previous,
    PlayPause,
    Play,
    Pause,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaKeyInfo {
    Author(String),
    Title(String),
    Playing,
    Paused,
    Shutdown,
}

// Linux-specific imports and implementation
#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use tracing::debug;
    use zbus::Connection;
    use zbus::interface;

    #[derive(Clone, Debug, Default)]
    pub struct PlayState {
        is_playing: bool,
        title: String,
        author: String,
    }

    /// Main MPRIS interface implementation
    pub struct MainInterface {
        event_sender: mpsc::Sender<MediaKeyEvent>,
    }

    impl MainInterface {
        fn new(event_sender: mpsc::Sender<MediaKeyEvent>) -> Self {
            MainInterface { event_sender }
        }
    }

    #[interface(name = "org.mpris.MediaPlayer2")]
    impl MainInterface {
        #[zbus(property)]
        fn identity(&self) -> String {
            "Oldplay".to_string()
        }

        #[zbus(property)]
        fn can_quit(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_raise(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn has_track_list(&self) -> bool {
            false
        }

        fn quit(&self) -> zbus::fdo::Result<()> {
            Ok(())
        }

        fn raise(&self) -> zbus::fdo::Result<()> {
            Ok(())
        }
    }

    /// MPRIS Media Player interface implementation
    pub struct MediaPlayer {
        play_state: Arc<Mutex<PlayState>>,
        event_sender: mpsc::Sender<MediaKeyEvent>,
    }

    impl MediaPlayer {
        fn new(
            play_state: Arc<Mutex<PlayState>>,
            event_sender: mpsc::Sender<MediaKeyEvent>,
        ) -> Self {
            MediaPlayer {
                play_state,
                event_sender,
            }
        }
    }

    #[interface(name = "org.mpris.MediaPlayer2.Player")]
    impl MediaPlayer {
        #[zbus(property)]
        fn playback_status(&self) -> String {
            if let Ok(ps) = self.play_state.lock() {
                if ps.is_playing {
                    "Playing".to_string()
                } else {
                    "Paused".to_string()
                }
            } else {
                "Stopped".to_string()
            }
        }

        #[zbus(property)]
        fn rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn metadata(&self) -> std::collections::HashMap<String, zbus::zvariant::Value<'_>> {
            use zbus::zvariant::ObjectPath;
            let mut metadata = std::collections::HashMap::new();
            if let Ok(track_id) = ObjectPath::try_from("/org/mpris/MediaPlayer2/Track/1") {
                metadata.insert(
                    "mpris:trackid".to_string(),
                    zbus::zvariant::Value::new(track_id),
                );
            }
            if let Ok(ps) = self.play_state.lock() {
                metadata.insert(
                    "xesam:title".to_string(),
                    zbus::zvariant::Value::new(ps.title.to_string()),
                );
                metadata.insert(
                    "xesam:artist".to_string(),
                    zbus::zvariant::Value::Array(zbus::zvariant::Array::from(vec![
                        ps.author.to_string(),
                    ])),
                );
            }
            metadata
        }

        #[zbus(property)]
        fn volume(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn position(&self) -> i64 {
            0
        }

        #[zbus(property)]
        fn minimum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn maximum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_play(&self) -> bool {
            if let Ok(play_state) = self.play_state.lock() {
                debug!("can_play {}", !play_state.is_playing);
                !play_state.is_playing
            } else {
                true
            }
        }

        #[zbus(property)]
        fn can_pause(&self) -> bool {
            if let Ok(play_state) = self.play_state.lock() {
                debug!("can_pause {}", play_state.is_playing);
                play_state.is_playing
            } else {
                true
            }
        }

        #[zbus(property)]
        fn can_seek(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn can_control(&self) -> bool {
            true
        }

        fn next(&self) -> zbus::fdo::Result<()> {
            debug!("[MPRIS] Next pressed");
            let _ = self.event_sender.send(MediaKeyEvent::Next);
            Ok(())
        }

        fn previous(&self) -> zbus::fdo::Result<()> {
            debug!("[MPRIS] Previous pressed");
            let _ = self.event_sender.send(MediaKeyEvent::Previous);
            Ok(())
        }

        fn play_pause(&self) -> zbus::fdo::Result<()> {
            debug!("[MPRIS] PlayPause pressed");
            let _ = self.event_sender.send(MediaKeyEvent::PlayPause);
            Ok(())
        }

        fn play(&self) -> zbus::fdo::Result<()> {
            debug!("[MPRIS] Play pressed");
            let _ = self.event_sender.send(MediaKeyEvent::Play);
            Ok(())
        }

        fn pause(&self) -> zbus::fdo::Result<()> {
            debug!("[MPRIS] Pause pressed");
            let _ = self.event_sender.send(MediaKeyEvent::Pause);
            Ok(())
        }

        fn stop(&self) -> zbus::fdo::Result<()> {
            let _ = self.event_sender.send(MediaKeyEvent::Stop);
            Ok(())
        }
    }

    /// Start the MPRIS listener in a background thread
    /// Returns (shutdown_sender, event_receiver, service_name)
    pub fn start_with_name(
        service_name: &str,
    ) -> (
        mpsc::Sender<MediaKeyInfo>,
        mpsc::Receiver<MediaKeyEvent>,
        String,
    ) {
        let play_state = Arc::new(Mutex::new(PlayState::default()));
        let (event_sender, event_receiver) = mpsc::channel();
        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let state_clone = play_state.clone();
        let service_name_owned = service_name.to_string();
        let service_name_for_thread = service_name_owned.clone();

        debug!("[MPRIS] Starting");

        thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async {
                match setup_mpris(
                    shutdown_receiver,
                    state_clone,
                    &service_name_for_thread,
                    event_sender,
                )
                .await
                {
                    Ok(_) => {
                        debug!("[MPRIS] Listener started successfully");
                    }
                    Err(e) => {
                        debug!("[MPRIS] Error starting listener: {}", e);
                    }
                }
            });
        });

        (shutdown_sender, event_receiver, service_name_owned)
    }

    async fn setup_mpris(
        event_receiver: mpsc::Receiver<MediaKeyInfo>,
        play_state: Arc<Mutex<PlayState>>,
        service_name: &str,
        event_sender: mpsc::Sender<MediaKeyEvent>,
    ) -> Result<(), zbus::Error> {
        let connection = Connection::session().await?;

        // Request the MPRIS service name
        connection.request_name(service_name).await?;

        // Register both the main interface and the player interface at the standard MPRIS path
        let main = MainInterface::new(event_sender.clone());
        let player = MediaPlayer::new(play_state.clone(), event_sender);

        connection
            .object_server()
            .at("/org/mpris/MediaPlayer2", main)
            .await?;

        connection
            .object_server()
            .at("/org/mpris/MediaPlayer2", player)
            .await?;

        debug!("[MPRIS] Registered as {}", service_name);

        // Keep the connection alive until shutdown is signaled
        loop {
            if let Ok(x) = event_receiver.try_recv()
                && let Ok(mut ps) = play_state.lock()
            {
                match x {
                    MediaKeyInfo::Shutdown => break,
                    MediaKeyInfo::Playing => ps.is_playing = true,
                    MediaKeyInfo::Paused => ps.is_playing = false,
                    MediaKeyInfo::Title(title) => ps.title = title,
                    MediaKeyInfo::Author(author) => ps.author = author,
                }
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        connection.release_name(service_name).await?;
        debug!("[MPRIS] Listener stopped");

        Ok(())
    }
}

// Public API - works on all platforms
#[cfg(target_os = "linux")]
pub fn start() -> (mpsc::Sender<MediaKeyInfo>, mpsc::Receiver<MediaKeyEvent>) {
    let (sender, receiver, _) = linux_impl::start_with_name("org.mpris.MediaPlayer2.oldplay");
    (sender, receiver)
}

#[cfg(not(target_os = "linux"))]
pub fn start() -> (mpsc::Sender<MediaKeyInfo>, mpsc::Receiver<MediaKeyEvent>) {
    use std::thread;
    use std::time::Duration;

    // Create channels for communication
    let (info_sender, info_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();

    // Spawn a dummy thread that keeps the channels alive by reading from them
    thread::spawn(move || {
        loop {
            // Try to receive info messages (like play state updates)
            match info_receiver.try_recv() {
                Ok(MediaKeyInfo::Shutdown) => break,
                Ok(_) => {} // Ignore other messages
                Err(_) => {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        // event_sender is held here and dropped when thread exits
        drop(event_sender);
    });

    (info_sender, event_receiver)
}
