//! End-to-end test: two engines in one process talk over real TLS on
//! localhost. Covers pairing (including a wrong code), layout sync, edge
//! switching, input forwarding and returning to the local machine.

use std::sync::Arc;
use std::time::Duration;

use smk_core::engine::{Command, EngineHandle, StartOptions, UiPairing, UiState};
use smk_core::input::CapturedEvent;
use smk_core::layout::{Layout, Placement};
use smk_core::protocol::{InputEvent, Rect};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

async fn engine(port: u16, screen: Rect) -> (EngineHandle, UnboundedReceiver<InputEvent>) {
    let dir = std::env::temp_dir().join(format!("smk-test-{}", uuid::Uuid::new_v4()));
    let (tx, rx) = unbounded_channel();
    let opts = StartOptions {
        config_dir: dir,
        port: Some(port),
        os_input: false,
        clipboard: false,
        discovery: false,
        screen: Some(screen),
        inject_sink: Some(tx),
    };
    let h = smk_core::start(opts, Arc::new(|| {})).await.expect("engine starts");
    (h, rx)
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

async fn next_event(rx: &mut UnboundedReceiver<InputEvent>) -> InputEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event in time")
        .expect("channel open")
}

fn peer<'a>(s: &'a UiState, name: &str) -> Option<&'a smk_core::engine::UiPeer> {
    s.peers.iter().find(|p| p.name == name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_sync_layout_and_forward_input() {
    let screen_a = Rect { x: 0.0, y: 0.0, w: 1000.0, h: 800.0 };
    let screen_b = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
    let (a, _rx_a) = engine(25811, screen_a).await;
    let (b, mut rx_b) = engine(25812, screen_b).await;
    a.send(Command::SetName("alpha".into()));
    b.send(Command::SetName("beta".into()));
    wait_until("names applied", || a.state().me.name == "alpha" && b.state().me.name == "beta").await;

    // Connect A -> B by address.
    a.send(Command::Connect("127.0.0.1:25812".into()));
    wait_until("both see each other", || {
        peer(&a.state(), "beta").is_some_and(|p| p.connected)
            && peer(&b.state(), "alpha").is_some_and(|p| p.connected)
    })
    .await;
    assert!(!peer(&a.state(), "beta").unwrap().trusted);

    // Pairing: B shows a code, A types it.
    let b_id = b.state().me.node_id;
    let a_id = a.state().me.node_id;
    a.send(Command::Pair(b_id));
    let mut code = String::new();
    wait_until("B shows code", || {
        if let Some(UiPairing::ShowCode { code: c }) = peer(&b.state(), "alpha").and_then(|p| p.pairing.clone()) {
            code = c;
            true
        } else {
            false
        }
    })
    .await;

    // A wrong code is rejected locally.
    let wrong = if code == "000000" { "111111" } else { "000000" };
    a.send(Command::SubmitCode(b_id, wrong.into()));
    wait_until("wrong code error", || {
        matches!(
            peer(&a.state(), "beta").and_then(|p| p.pairing.clone()),
            Some(UiPairing::EnterCode { error: Some(_) })
        )
    })
    .await;

    a.send(Command::SubmitCode(b_id, code.clone()));
    wait_until("both trusted", || {
        peer(&a.state(), "beta").is_some_and(|p| p.trusted)
            && peer(&b.state(), "alpha").is_some_and(|p| p.trusted)
    })
    .await;

    // Layout: B to the right of A, then check it reached B.
    a.send(Command::SetLayout(Layout {
        version: 0,
        placements: vec![
            Placement { node_id: a_id, name: "alpha".into(), x: 0, y: 0 },
            Placement { node_id: b_id, name: "beta".into(), x: 1, y: 0 },
        ],
    }));
    wait_until("layout synced to B", || {
        let l = b.state().layout;
        l.position(a_id) == Some((0, 0)) && l.position(b_id) == Some((1, 0))
    })
    .await;

    // Push against A's right edge -> cursor enters B at its left edge,
    // at the same relative height (400/800 -> 540/1080).
    a.simulate_input(CapturedEvent::Motion { x: 999.0, y: 400.0, dx: 5.0, dy: 0.0 });
    match next_event(&mut rx_b).await {
        InputEvent::MouseMove { x, y } => {
            assert!(x <= 2.0, "entered at left edge, got x={x}");
            assert!((y - 540.0).abs() < 1.0, "relative height kept, got y={y}");
        }
        other => panic!("expected MouseMove, got {other:?}"),
    }
    wait_until("A is controlling B", || a.state().controlling.as_deref() == Some("beta")).await;

    // Motion and keys are forwarded.
    a.simulate_input(CapturedEvent::Motion { x: f64::NAN, y: f64::NAN, dx: 100.0, dy: 20.0 });
    match next_event(&mut rx_b).await {
        InputEvent::MouseMove { x, y } => {
            assert!((x - 101.0).abs() < 1.5 && (y - 560.0).abs() < 1.5, "got {x},{y}");
        }
        other => panic!("expected MouseMove, got {other:?}"),
    }
    a.simulate_input(CapturedEvent::Key { code: 30, down: true });
    assert_eq!(next_event(&mut rx_b).await, InputEvent::Key { code: 30, down: true });

    // Moving back past B's left edge returns control to A and releases the
    // key still held on B.
    a.simulate_input(CapturedEvent::Motion { x: f64::NAN, y: f64::NAN, dx: -500.0, dy: 0.0 });
    assert_eq!(next_event(&mut rx_b).await, InputEvent::Key { code: 30, down: false });
    wait_until("control back on A", || a.state().controlling.is_none()).await;
    wait_until("B no longer controlled", || b.state().controlled_by.is_none()).await;

    // Unpairing removes trust on A and B's machine from A's layout.
    a.send(Command::Unpair(b_id));
    wait_until("unpaired", || {
        !peer(&a.state(), "beta").is_some_and(|p| p.trusted) && a.state().layout.position(b_id).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejecting_pairing_clears_both_sides() {
    let screen = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 720.0 };
    let (a, _ra) = engine(25821, screen).await;
    let (b, _rb) = engine(25822, screen).await;
    a.send(Command::SetName("alpha".into()));
    b.send(Command::SetName("beta".into()));
    a.send(Command::Connect("127.0.0.1:25822".into()));
    wait_until("connected", || {
        peer(&a.state(), "beta").is_some_and(|p| p.connected) && peer(&b.state(), "alpha").is_some_and(|p| p.connected)
    })
    .await;
    let (a_id, b_id) = (a.state().me.node_id, b.state().me.node_id);
    a.send(Command::Pair(b_id));
    wait_until("B shows code", || peer(&b.state(), "alpha").is_some_and(|p| p.pairing.is_some())).await;

    // B rejects: A must stop waiting for a code.
    b.send(Command::CancelPair(a_id));
    wait_until("A no longer pairing", || peer(&a.state(), "beta").is_some_and(|p| p.pairing.is_none())).await;
    assert!(!peer(&a.state(), "beta").unwrap().trusted);
    assert!(!peer(&b.state(), "alpha").unwrap().trusted);
}
