//! Local native call-engine lifecycle check; run only through bin/headless.
//! Creates no Telegram session, transport connection, or capture device.
use ntgcalls::NTgCalls;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    gtk4::init().expect("private headless GTK display");
    assert!(!NTgCalls::ping().expect("native engine responds").is_empty());
    NTgCalls::get_protocol().expect("native call protocol");
    for _ in 0..3 {
        let engine = NTgCalls::new();
        assert!(engine.calls().await.expect("initial call list").is_empty());
        engine
            .create_p2p_call(1001)
            .await
            .expect("create local call state");
        assert!(
            engine
                .calls()
                .await
                .expect("created call list")
                .contains_key(&1001)
        );
        engine.stop(1001).await.expect("release local call state");
        assert!(engine.calls().await.expect("final call list").is_empty());
        // Exercise GTK/GLib after the native engine has started its threads.
        let label = gtk4::Label::new(Some("native call lifecycle"));
        assert_eq!(label.label(), "native call lifecycle");
        drop(engine);
    }
    println!("native call engine: 3 local create/list/stop cycles PASS");
}
