//! End-to-end test: two engines in one process talk over real TLS on
//! localhost. Covers pairing (including a wrong code), layout sync, edge
//! switching, input forwarding and returning to the local machine.

use std::sync::Arc;
use std::time::Duration;

use smk_core::engine::{Command, EngineHandle, StartOptions, UiPairing, UiState};
use smk_core::input::CapturedEvent;
use smk_core::keymap::ev;
use smk_core::layout::{Layout, Placement};
use smk_core::protocol::{InputEvent, Rect};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// A test's temporary folder, deleted when the test ends (even if it fails).
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("smk-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn engine(tmp: &Tmp, port: u16, screen: Rect) -> (EngineHandle, UnboundedReceiver<InputEvent>) {
    let dir = tmp.join(&format!("node-{port}"));
    let downloads = dir.join("downloads");
    let (tx, rx) = unbounded_channel();
    let opts = StartOptions {
        config_dir: dir,
        port: Some(port),
        os_input: false,
        clipboard: false,
        discovery: false,
        screen: Some(screen),
        inject_sink: Some(tx),
        read_only: false,
        download_root: Some(downloads),
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
    let tmp = Tmp::new();
    let screen_a = Rect { x: 0.0, y: 0.0, w: 1000.0, h: 800.0 };
    let screen_b = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
    let (a, _rx_a) = engine(&tmp, 25811, screen_a).await;
    let (b, mut rx_b) = engine(&tmp, 25812, screen_b).await;
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
    let tmp = Tmp::new();
    let screen = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 720.0 };
    let (a, _ra) = engine(&tmp, 25821, screen).await;
    let (b, _rb) = engine(&tmp, 25822, screen).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_or_closed_machines_decline_pairing() {
    let tmp = Tmp::new();
    let screen = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 720.0 };
    let (a, _ra) = engine(&tmp, 25851, screen).await;
    let (b, _rb) = engine(&tmp, 25852, screen).await;
    a.send(Command::SetName("alpha".into()));
    b.send(Command::SetName("beta".into()));
    a.send(Command::Connect("127.0.0.1:25852".into()));
    wait_until("connected", || {
        peer(&a.state(), "beta").is_some_and(|p| p.connected) && peer(&b.state(), "alpha").is_some_and(|p| p.connected)
    })
    .await;
    let (a_id, b_id) = (a.state().me.node_id, b.state().me.node_id);
    let declined = |s: &UiState| peer(s, "beta").is_some_and(|p| matches!(p.pairing, Some(UiPairing::Declined)));

    // B blocks A: A's request is declined, B never shows a code, and A
    // disappears from B's list into the blocked list.
    b.send(Command::Block(a_id));
    wait_until("A blocked on B", || b.state().blocked.iter().any(|x| x.node_id == a_id && x.online)).await;
    assert!(peer(&b.state(), "alpha").is_none());
    a.send(Command::Pair(b_id));
    wait_until("A sees decline", || declined(&a.state())).await;
    assert!(b.state().peers.iter().all(|p| p.pairing.is_none()));

    // Unblocked, but pairing requests switched off: still declined.
    a.send(Command::CancelPair(b_id));
    b.send(Command::Unblock(a_id));
    let mut settings = b.state().settings.clone();
    settings.accept_pairing = false;
    b.send(Command::SetSettings(settings.clone()));
    wait_until("A back in B's list", || peer(&b.state(), "alpha").is_some() && b.state().blocked.is_empty()).await;
    wait_until("A idle", || peer(&a.state(), "beta").is_some_and(|p| p.pairing.is_none())).await;
    a.send(Command::Pair(b_id));
    wait_until("A sees decline again", || declined(&a.state())).await;

    // Switched back on: B shows the code as usual.
    a.send(Command::CancelPair(b_id));
    settings.accept_pairing = true;
    b.send(Command::SetSettings(settings));
    wait_until("A idle again", || peer(&a.state(), "beta").is_some_and(|p| p.pairing.is_none())).await;
    a.send(Command::Pair(b_id));
    wait_until("B shows code", || peer(&b.state(), "alpha").is_some_and(|p| matches!(p.pairing, Some(UiPairing::ShowCode { .. })))).await;
}

/// Connect and pair two engines, with B placed to the right of A.
async fn paired(tmp: &Tmp, pa: u16, pb: u16) -> (EngineHandle, EngineHandle, UnboundedReceiver<InputEvent>) {
    let screen = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 720.0 };
    let (a, _ra) = engine(tmp, pa, screen).await;
    let (b, rb) = engine(tmp, pb, screen).await;
    a.send(Command::SetName("alpha".into()));
    b.send(Command::SetName("beta".into()));
    a.send(Command::Connect(format!("127.0.0.1:{pb}")));
    wait_until("connected", || {
        peer(&a.state(), "beta").is_some_and(|p| p.connected) && peer(&b.state(), "alpha").is_some_and(|p| p.connected)
    })
    .await;
    let (a_id, b_id) = (a.state().me.node_id, b.state().me.node_id);
    a.send(Command::Pair(b_id));
    let mut code = String::new();
    wait_until("code", || match peer(&b.state(), "alpha").and_then(|p| p.pairing.clone()) {
        Some(UiPairing::ShowCode { code: c }) => {
            code = c;
            true
        }
        _ => false,
    })
    .await;
    a.send(Command::SubmitCode(b_id, code));
    wait_until("trusted", || peer(&b.state(), "alpha").is_some_and(|p| p.trusted)).await;
    a.send(Command::SetLayout(Layout {
        version: 0,
        placements: vec![
            Placement { node_id: a_id, name: "alpha".into(), x: 0, y: 0 },
            Placement { node_id: b_id, name: "beta".into(), x: 1, y: 0 },
        ],
    }));
    wait_until("layout", || b.state().layout.position(b_id) == Some((1, 0))).await;
    (a, b, rb)
}

fn transfer_on(s: &UiState, incoming: bool) -> Option<smk_core::engine::UiTransfer> {
    s.transfers.iter().find(|t| t.incoming == incoming).cloned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copied_folder_streams_only_when_pasted() {
    let tmp = Tmp::new();
    use smk_core::engine::TransferState;
    use smk_core::keymap::ev;
    let (a, b, mut rb) = paired(&tmp, 25831, 25832).await;

    // A folder with a multi-chunk file.
    let src = tmp.join("src").join("Album");
    std::fs::create_dir_all(src.join("raw")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(src.join("raw/photo.bin"), &big).unwrap();
    std::fs::write(src.join("notes.txt"), b"hi").unwrap();

    a.send(Command::OfferFiles(vec![src.clone()]));
    // B only gets the list for now.
    wait_until("B has a pending offer", || {
        transfer_on(&b.state(), true).is_some_and(|t| t.state == TransferState::Pending && t.label == "Album" && t.files == 2)
    })
    .await;

    // Moving the cursor onto B doesn't fetch anything…
    a.simulate_input(CapturedEvent::Motion { x: 1279.0, y: 300.0, dx: 5.0, dy: 0.0 });
    wait_until("controlling B", || a.state().controlling.as_deref() == Some("beta")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(transfer_on(&b.state(), true).unwrap().state, TransferState::Pending);
    while rb.try_recv().is_ok() {}

    // …pasting there does, and the paste keystroke itself is held back.
    let m = if cfg!(target_os = "macos") { ev::LEFTMETA } else { ev::LEFTCTRL };
    for (code, down) in [(m, true), (ev::V, true), (ev::V, false), (m, false)] {
        a.simulate_input(CapturedEvent::Key { code, down });
    }
    wait_until("download finished", || transfer_on(&b.state(), true).is_some_and(|t| t.state == TransferState::Done)).await;
    wait_until("upload shown as done", || transfer_on(&a.state(), false).is_some_and(|t| t.state == TransferState::Done)).await;
    let mut keys = Vec::new();
    while let Ok(e) = rb.try_recv() {
        if let InputEvent::Key { code, down } = e {
            keys.push((code, down));
        }
    }
    assert_eq!(keys, vec![(m, true), (m, false)], "V must not reach B");

    let t = transfer_on(&b.state(), true).unwrap();
    assert_eq!(t.done, t.total);
    assert_eq!(t.total, big.len() as u64 + 2);
    assert_eq!(t.paths.len(), 1);
    assert_eq!(std::fs::read(std::path::Path::new(&t.paths[0]).join("raw/photo.bin")).unwrap(), big);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_to_folder_keeps_existing_files() {
    let tmp = Tmp::new();
    use smk_core::engine::TransferState;
    let (a, b, _rb) = paired(&tmp, 25861, 25862).await;
    let root = tmp.join("save");
    let src = root.join("src");
    let dst = root.join("external disk");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(src.join("movie.mov"), b"new movie").unwrap();
    std::fs::write(dst.join("movie.mov"), b"old movie").unwrap();

    a.send(Command::OfferFiles(vec![src.join("movie.mov")]));
    let mut id = None;
    wait_until("offer", || {
        id = transfer_on(&b.state(), true).map(|t| t.id);
        id.is_some()
    })
    .await;
    b.send(Command::SaveTransferTo(id.unwrap(), dst.clone()));
    wait_until("saved", || transfer_on(&b.state(), true).is_some_and(|t| t.state == TransferState::Done)).await;
    let t = transfer_on(&b.state(), true).unwrap();
    assert_eq!(t.paths, vec![dst.join("movie 2.mov").display().to_string()]);
    assert_eq!(std::fs::read(dst.join("movie.mov")).unwrap(), b"old movie");
    assert_eq!(std::fs::read(dst.join("movie 2.mov")).unwrap(), b"new movie");
    // Nothing left behind but the two movies.
    assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_download_stops_it() {
    let tmp = Tmp::new();
    use smk_core::engine::TransferState;
    let (a, b, _rb) = paired(&tmp, 25841, 25842).await;
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("huge.bin");
    // Big sparse file: instant to create, far too big to finish in the test.
    // Capped by free space, since the receiver refuses what won't fit (CI
    // runners can have less than 20 GB free). NTFS doesn't create sparse
    // files, so the source itself also uses that much space there.
    let free = fs4::available_space(&src).unwrap();
    let size = (free / 4).min(20_000_000_000);
    std::fs::File::create(&file).unwrap().set_len(size).unwrap();

    a.send(Command::OfferFiles(vec![file]));
    let mut id = None;
    wait_until("offer", || {
        id = transfer_on(&b.state(), true).map(|t| t.id);
        id.is_some()
    })
    .await;
    b.send(Command::StartTransfer(id.unwrap()));
    wait_until("some progress", || {
        let t = transfer_on(&b.state(), true);
        assert!(t.as_ref().is_none_or(|t| t.error.is_none()), "transfer failed: {:?}", t.map(|t| t.error.clone()));
        t.is_some_and(|t| t.done > 0)
    })
    .await;
    b.send(Command::CancelTransfer(id.unwrap()));
    wait_until("cancelled", || transfer_on(&b.state(), true).is_some_and(|t| t.state == TransferState::Cancelled)).await;
    let done = transfer_on(&b.state(), true).unwrap().done;
    assert!(done < size);
}

#[tokio::test(flavor = "multi_thread")]
async fn pointer_speed_scales_remote_motion() {
    let tmp = Tmp::new();
    let (a, _b, mut rb) = paired(&tmp, 25871, 25872).await;
    for speed in [1.0, 3.0] {
        let mut settings = a.state().settings.clone();
        settings.pointer_speed = speed;
        a.send(Command::SetSettings(settings));
        wait_until("speed applied", || a.state().settings.pointer_speed == speed).await;
        a.simulate_input(CapturedEvent::Motion { x: 1279.0, y: 300.0, dx: 5.0, dy: 0.0 });
        let InputEvent::MouseMove { x: entry, .. } = next_event(&mut rb).await else { panic!("expected entry move") };
        a.simulate_input(CapturedEvent::Motion { x: f64::NAN, y: f64::NAN, dx: 100.0, dy: 0.0 });
        let InputEvent::MouseMove { x, .. } = next_event(&mut rb).await else { panic!("expected move") };
        assert!(((x - entry) - 100.0 * speed).abs() < 1.5, "speed {speed}: moved {}", x - entry);
        a.send(Command::ReturnHome);
        wait_until("home", || a.state().controlling.is_none()).await;
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), rb.recv()).await {}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_request_is_answered_with_an_error_on_a_machine_that_is_not_a_phone() {
    let tmp = Tmp::new();
    let (a, b, _rb) = paired(&tmp, 25881, 25882).await;
    a.send(Command::RequestSnapshot(Some(b.state().me.node_id)));
    wait_until("error reaches A", || a.state().log.iter().any(|l| l.text.contains("could not take a snapshot"))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_settings_are_shared_between_paired_machines() {
    let tmp = Tmp::new();
    let (a, b, _rb) = paired(&tmp, 25891, 25892).await;

    let mut settings = a.state().settings.clone();
    settings.pointer_speed = 2.5;
    settings.edge_resistance_px = 30.0;
    a.send(Command::SetSettings(settings));
    wait_until("B follows A", || {
        let s = b.state().settings;
        s.pointer_speed == 2.5 && s.edge_resistance_px == 30.0
    })
    .await;

    // The newest edit wins in both directions, and a UI that doesn't know the
    // version (it never sends one) can't roll it back.
    let mut settings = b.state().settings.clone();
    settings.pointer_speed = 0.5;
    settings.shared_version = 0;
    b.send(Command::SetSettings(settings));
    wait_until("A follows B", || a.state().settings.pointer_speed == 0.5).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_alt_s_asks_for_a_phone_screenshot() {
    let tmp = Tmp::new();
    let (a, _b, _rb) = paired(&tmp, 25893, 25894).await;
    for (code, down) in [(ev::LEFTCTRL, true), (ev::LEFTALT, true), (ev::S, true), (ev::S, false)] {
        a.simulate_input(CapturedEvent::Key { code, down });
    }
    // Neither machine is a phone, so the request finds nobody and says so.
    wait_until("request noticed", || a.state().log.iter().any(|l| l.text.contains("No connected phone"))).await;
}

/// `x` connects to `y` and the two pair, `y` showing the code.
async fn pair_machines(x: &EngineHandle, y: &EngineHandle, y_port: u16, x_name: &str, y_name: &str) {
    x.send(Command::Connect(format!("127.0.0.1:{y_port}")));
    wait_until("connected", || {
        peer(&x.state(), y_name).is_some_and(|p| p.connected) && peer(&y.state(), x_name).is_some_and(|p| p.connected)
    })
    .await;
    let y_id = y.state().me.node_id;
    x.send(Command::Pair(y_id));
    let mut code = String::new();
    wait_until("code", || match peer(&y.state(), x_name).and_then(|p| p.pairing.clone()) {
        Some(UiPairing::ShowCode { code: c }) => {
            code = c;
            true
        }
        _ => false,
    })
    .await;
    x.send(Command::SubmitCode(y_id, code));
    wait_until("trusted", || peer(&y.state(), x_name).is_some_and(|p| p.trusted)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn machines_paired_with_the_same_peer_find_each_other() {
    let tmp = Tmp::new();
    let screen = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 720.0 };
    let (hub, _r1) = engine(&tmp, 25895, screen).await;
    let (left, _r2) = engine(&tmp, 25896, screen).await;
    let (right, _r3) = engine(&tmp, 25897, screen).await;
    hub.send(Command::SetName("hub".into()));
    left.send(Command::SetName("left".into()));
    right.send(Command::SetName("right".into()));
    wait_until("names", || hub.state().me.name == "hub" && left.state().me.name == "left" && right.state().me.name == "right").await;

    pair_machines(&left, &hub, 25895, "left", "hub").await;
    pair_machines(&right, &hub, 25895, "right", "hub").await;

    // Neither knows the other's address, but the hub does and passes it on.
    wait_until("left and right meet", || {
        peer(&left.state(), "right").is_some_and(|p| p.connected) && peer(&right.state(), "left").is_some_and(|p| p.connected)
    })
    .await;
    assert!(!peer(&left.state(), "right").unwrap().trusted, "meeting is not trusting");
}
