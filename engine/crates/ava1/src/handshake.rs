//! The session handshake: Noise XX in frames Hs1..Hs3, then Welcome (SPEC.md §5).
use tokio::io::{AsyncRead, AsyncWrite};

use crate::conn::{Frame, FrameReader, FrameWriter};
use crate::gen::{self, ClientInfo, HelloInfo, Hs1, Hs2, Hs3, ServerInfo, Welcome};
use crate::keys::{self, Handshake, Identity, SessionKeys};
use crate::launch::{self, LaunchSecret};
use crate::wire::{FrameMessage, Message};
use crate::Ava1Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingState {
    /// The server does not know us yet and must accept a PairConfirm.
    pub server_must_confirm: bool,
}

pub struct Established {
    pub keys: SessionKeys,
    pub session_id: [u8; 16],
    pub peer_key: [u8; 32],
    pub peer_name: String,
    /// What the peer advertised in its info message (e.g. `CAP_DATA_PLANE`).
    pub peer_caps: u64,
    /// `Some` until both devices have accepted each other.
    pub pairing: Option<PairingState>,
    /// Client side: the server proved a launch token this side issued (SPEC.md §5.2),
    /// so it is trusted without pairing; the caller stores its key.
    pub launched: bool,
}

/// What a server decides about a client once the handshake has shown its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A paired device.
    Known,
    /// Unknown, but the pairing window is open and there is room: Welcome with
    /// `knows_you = 0`, then wait for its PairConfirm.
    Pairing,
    /// Refused with this sealed `Error`, then closed.
    Refuse(u16, &'static str),
}

pub(crate) fn refused(f: &Frame) -> Ava1Error {
    match f.decode::<gen::Error>() {
        Ok(e) => Ava1Error::Refused {
            code: e.code,
            message: e.message,
        },
        Err(e) => e,
    }
}

pub async fn refuse<W: AsyncWrite + Unpin>(w: &mut FrameWriter<W>, code: u16, message: &str) {
    let _ = w
        .send_msg(
            0,
            &gen::Error {
                code,
                message: message.into(),
            },
        )
        .await;
}

pub async fn client<R, W>(
    r: &mut FrameReader<R>,
    w: &mut FrameWriter<W>,
    me: &Identity,
    my_name: &str,
    knows: impl Fn(&[u8; 32]) -> bool,
    caps: u64,
) -> Result<Established, Ava1Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    client_expecting(r, w, me, my_name, None, knows, caps).await
}

/// `client`, refusing any server whose static key is not `expected` (when given): the
/// right address can be the wrong console. The check happens on message 2, before this
/// side has revealed its own key or name.
pub async fn client_expecting<R, W>(
    r: &mut FrameReader<R>,
    w: &mut FrameWriter<W>,
    me: &Identity,
    my_name: &str,
    expected: Option<[u8; 32]>,
    knows: impl Fn(&[u8; 32]) -> bool,
    caps: u64,
) -> Result<Established, Ava1Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    client_launched(r, w, me, my_name, expected, knows, |_, _| false, caps).await
}

/// Whether a `Welcome` proves we launched that server (SPEC.md §5.2). Three things have
/// to hold, and each rules out a different impostor: the server is one we do not already
/// know (a known one needs no proof), it says it knows us (a server that does not know the
/// client was never given a proof, so one that sends a proof anyway is not to be trusted),
/// and its proof matches a token we issued and have not expired.
fn proves_our_launch(
    known: bool,
    welcome: &Welcome,
    hash: &[u8; 64],
    recognises: impl Fn(&[u8; 64], &[u8; 16]) -> bool,
) -> bool {
    !known && welcome.knows_you != 0 && welcome.launch_proof.is_some_and(|p| recognises(hash, &p))
}

/// `client_expecting`, also trusting a server it does not know whose Welcome carries a
/// launch proof that `launched(h, proof)` recognises (SPEC.md §5.2) — the helper this
/// side launched. Such a session is paired at once (`Established::launched`).
#[allow(clippy::too_many_arguments)] // the closures are the config surface; caps is last
pub async fn client_launched<R, W>(
    r: &mut FrameReader<R>,
    w: &mut FrameWriter<W>,
    me: &Identity,
    my_name: &str,
    expected: Option<[u8; 32]>,
    knows: impl Fn(&[u8; 32]) -> bool,
    launched: impl Fn(&[u8; 64], &[u8; 16]) -> bool,
    caps: u64,
) -> Result<Established, Ava1Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let v = gen::PROTOCOL_VERSION;
    let mut hs = Handshake::initiator(me)?;
    let hello = HelloInfo {
        version_min: v,
        version_max: v,
        caps,
    }
    .to_bytes()?;
    w.send_msg(
        0,
        &Hs1 {
            noise: hs.write(&hello)?,
        },
    )
    .await?;
    let f = r.recv().await?;
    if f.ty == gen::Error::TYPE {
        return Err(refused(&f));
    }
    let m2: Hs2 = f.decode()?;
    let info = match ServerInfo::decode(&hs.read(&m2.noise)?) {
        Ok(i) => i,
        Err(e) => {
            // A server that omits the pairing commitment is refused (SPEC.md §4.6): an
            // optional field could be stripped by a man in the middle.
            refuse(w, gen::ERR_PROTOCOL, "bad ServerInfo").await;
            return Err(e.into());
        }
    };
    if info.version != v {
        return Err(Ava1Error::Version {
            min: info.version,
            max: info.version,
            ours: v,
        });
    }
    let peer_key = hs.remote_static().ok_or(Ava1Error::WeakKey)?;
    if expected.is_some_and(|k| k != peer_key) {
        return Err(Ava1Error::WrongPeer);
    }
    let nonce_c: [u8; 16] = keys::random_bytes()?;
    let ci = ClientInfo {
        nonce_c,
        name: Some(my_name.to_string()),
        token: None,
    }
    .to_bytes()?;
    w.send_msg(
        0,
        &Hs3 {
            noise: hs.write(&ci)?,
        },
    )
    .await?;
    let keys = hs.finish();
    w.set_key(keys::control_key(&keys.c2s));
    r.set_key(keys::control_key(&keys.s2c));
    let f = r.recv().await?;
    if f.ty == gen::Error::TYPE {
        return Err(refused(&f));
    }
    let welcome: Welcome = match f.decode() {
        Ok(wl) => wl,
        Err(e) => {
            refuse(w, gen::ERR_PROTOCOL, "bad Welcome").await;
            return Err(e);
        }
    };
    // The reveal must open the commitment from message 2 before anything is shown: this is
    // what stops a man in the middle from choosing its nonce after seeing ours.
    if keys::pair_commit(&welcome.nonce_s) != info.pair_commit {
        refuse(w, gen::ERR_PROTOCOL, "pairing commitment mismatch").await;
        return Err(Ava1Error::PairingCommitMismatch);
    }
    let known = knows(&peer_key);
    let launched = proves_our_launch(known, &welcome, &keys.hash, launched);
    let pairing = (!(known || launched) || welcome.knows_you == 0).then_some(PairingState {
        server_must_confirm: welcome.knows_you == 0,
    });
    Ok(Established {
        keys,
        session_id: info.session_id,
        peer_key,
        peer_name: info.name.unwrap_or_default(),
        peer_caps: info.caps,
        pairing,
        launched,
    })
}

pub async fn server<R, W>(
    r: &mut FrameReader<R>,
    w: &mut FrameWriter<W>,
    hs1_frame: Frame,
    me: &Identity,
    my_name: &str,
    admit: impl FnOnce(&[u8; 32]) -> Admission,
    caps: u64,
) -> Result<Established, Ava1Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    server_launched(r, w, hs1_frame, me, my_name, None, admit, caps).await
}

/// `server` for a node stamped with a launch token: a known client whose key is
/// `launch.key` gets the proof in its Welcome (SPEC.md §5.2). No other client does.
#[allow(clippy::too_many_arguments)] // the closure is the config surface; caps is last
pub async fn server_launched<R, W>(
    r: &mut FrameReader<R>,
    w: &mut FrameWriter<W>,
    hs1_frame: Frame,
    me: &Identity,
    my_name: &str,
    launch: Option<&LaunchSecret>,
    admit: impl FnOnce(&[u8; 32]) -> Admission,
    caps: u64,
) -> Result<Established, Ava1Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let v = gen::PROTOCOL_VERSION;
    let m1: Hs1 = hs1_frame.decode()?;
    let mut hs = Handshake::responder(me)?;
    let hello = HelloInfo::decode(&hs.read(&m1.noise)?)?;
    if hello.version_min > v || hello.version_max < v {
        refuse(
            w,
            gen::ERR_UNSUPPORTED_VERSION,
            "no protocol version in common",
        )
        .await;
        return Err(Ava1Error::Version {
            min: hello.version_min,
            max: hello.version_max,
            ours: v,
        });
    }
    let session_id: [u8; 16] = keys::random_bytes()?;
    let nonce_s: [u8; 16] = keys::random_bytes()?;
    let si = ServerInfo {
        version: v,
        caps,
        session_id,
        pair_commit: keys::pair_commit(&nonce_s),
        name: Some(my_name.to_string()),
    }
    .to_bytes()?;
    w.send_msg(
        0,
        &Hs2 {
            noise: hs.write(&si)?,
        },
    )
    .await?;
    let m3: Hs3 = r.recv().await?.decode()?;
    let ci = ClientInfo::decode(&hs.read(&m3.noise)?);
    let peer_key = hs.remote_static().ok_or(Ava1Error::WeakKey)?;
    let keys = hs.finish();
    w.set_key(keys::control_key(&keys.s2c));
    r.set_key(keys::control_key(&keys.c2s));
    // A client without its pairing nonce is refused (SPEC.md §4.6), sealed like every
    // frame from here on.
    let ci = match ci {
        Ok(ci) => ci,
        Err(e) => {
            refuse(w, gen::ERR_PROTOCOL, "bad ClientInfo").await;
            return Err(e.into());
        }
    };
    let known = match admit(&peer_key) {
        Admission::Known => true,
        Admission::Pairing => false,
        Admission::Refuse(code, message) => {
            refuse(w, code, message).await;
            return Err(if code == gen::ERR_PAIRING_CLOSED {
                Ava1Error::NotPaired
            } else {
                Ava1Error::Refused {
                    code,
                    message: message.into(),
                }
            });
        }
    };
    let launch_proof = launch
        .filter(|l| known && l.key == peer_key)
        .map(|l| launch::proof(&l.token, &keys.hash));
    w.send_msg(
        0,
        &Welcome {
            knows_you: u8::from(known),
            nonce_s,
            launch_proof,
        },
    )
    .await?;
    Ok(Established {
        pairing: (!known).then_some(PairingState {
            server_must_confirm: true,
        }),
        keys,
        session_id,
        peer_key,
        peer_name: ci.name.unwrap_or_default(),
        peer_caps: hello.caps,
        launched: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{FrameReader, FrameWriter};
    use tokio::io::{split, DuplexStream, ReadHalf, WriteHalf};

    type R = FrameReader<ReadHalf<DuplexStream>>;
    type W = FrameWriter<WriteHalf<DuplexStream>>;

    fn pipe() -> ((R, W), (R, W)) {
        let (a, b) = tokio::io::duplex(1 << 16);
        let ((ar, aw), (br, bw)) = (split(a), split(b));
        (
            (FrameReader::new(ar), FrameWriter::new(aw)),
            (FrameReader::new(br), FrameWriter::new(bw)),
        )
    }

    struct Ends {
        c: Result<Established, Ava1Error>,
        s: Result<Established, Ava1Error>,
        c_pub: [u8; 32],
        s_pub: [u8; 32],
    }

    async fn run(server_knows_client: bool, client_knows_server: bool, pairing_open: bool) -> Ends {
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (c_pub, s_pub) = (c_id.public(), s_id.public());
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let c_fut = async move {
            client(
                &mut cr,
                &mut cw,
                &c_id,
                "laptop",
                |k| client_knows_server && *k == s_pub,
                0,
            )
            .await
        };
        let s_fut = async move {
            let first = sr.recv().await?;
            server(
                &mut sr,
                &mut sw,
                first,
                &s_id,
                "console",
                |k| {
                    if server_knows_client && *k == c_pub {
                        Admission::Known
                    } else if pairing_open {
                        Admission::Pairing
                    } else {
                        Admission::Refuse(gen::ERR_PAIRING_CLOSED, "pairing is closed")
                    }
                },
                0,
            )
            .await
        };
        let (c, s) = tokio::join!(c_fut, s_fut);
        Ends { c, s, c_pub, s_pub }
    }

    /// A server stamped with (launcher key, token) the client `c_is_launcher` or not;
    /// the client accepts a proof iff `accept(h, proof)`. Returns the client's end and
    /// every proof it was shown.
    async fn run_launch(
        c_is_launcher: bool,
        accept: impl Fn(&[u8; 64], &[u8; 16]) -> bool,
    ) -> (Result<Established, Ava1Error>, Vec<([u8; 64], [u8; 16])>) {
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let c_pub = c_id.public();
        let launch = LaunchSecret {
            key: if c_is_launcher { c_pub } else { [0x33; 32] },
            token: [0x77; 16],
        };
        let seen = std::sync::Mutex::new(Vec::new());
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let c_fut = client_launched(
            &mut cr,
            &mut cw,
            &c_id,
            "laptop",
            None,
            |_| false,
            |h, p| {
                seen.lock().unwrap().push((*h, *p));
                accept(h, p)
            },
            0,
        );
        let s_fut = async {
            let first = sr.recv().await?;
            server_launched(
                &mut sr,
                &mut sw,
                first,
                &s_id,
                "console",
                Some(&launch),
                |_| Admission::Known,
                0,
            )
            .await
        };
        let (c, s) = tokio::join!(c_fut, s_fut);
        s.unwrap();
        (c, seen.into_inner().unwrap())
    }

    #[tokio::test]
    async fn a_launched_server_proves_its_token_and_needs_no_pairing() {
        let tok = [0x77u8; 16];
        let (c, seen) = run_launch(true, |h, p| crate::launch::proof(&tok, h) == *p).await;
        let c = c.unwrap();
        assert_eq!(c.pairing, None);
        assert!(c.launched);
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].0, c.keys.hash,
            "the proof is bound to this handshake"
        );
    }

    #[tokio::test]
    async fn a_proof_this_side_does_not_recognise_falls_back_to_pairing() {
        let (c, seen) = run_launch(true, |_, _| false).await;
        let c = c.unwrap();
        assert_eq!(seen.len(), 1);
        assert!(!c.launched);
        let p = c.pairing.expect("pairing still needed");
        assert!(
            !p.server_must_confirm,
            "the server already trusts its launcher"
        );
    }

    #[tokio::test]
    async fn only_the_launching_key_is_shown_a_proof() {
        let (c, seen) = run_launch(false, |_, _| true).await;
        assert!(seen.is_empty(), "another client key never receives a proof");
        assert!(c.unwrap().pairing.is_some());
    }

    #[tokio::test]
    async fn every_handshake_has_its_own_proof() {
        let (_, a) = run_launch(true, |_, _| true).await;
        let (_, b) = run_launch(true, |_, _| true).await;
        assert_ne!(a[0].1, b[0].1);
        let tok = [0x77u8; 16];
        // Replaying the first Welcome's proof into the second handshake fails.
        assert_ne!(crate::launch::proof(&tok, &b[0].0), a[0].1);
        assert_eq!(crate::launch::proof(&tok, &b[0].0), b[0].1);
    }

    #[test]
    fn a_proof_only_counts_from_a_server_that_says_it_knows_us() {
        let h = [9u8; 64];
        let accepts_anything = |_: &[u8; 64], _: &[u8; 16]| true;
        let welcome = |knows_you: u8, launch_proof: Option<[u8; 16]>| Welcome {
            knows_you,
            nonce_s: [0; 16],
            launch_proof,
        };
        assert!(proves_our_launch(
            false,
            &welcome(1, Some([1; 16])),
            &h,
            accepts_anything
        ));
        // knows_you = 0 while carrying a proof: the server never had one to send, so a
        // valid-looking proof must not skip the pairing. The live path cannot produce
        // this pair — known drives both — so this is the one that guards a refactor.
        assert!(!proves_our_launch(
            false,
            &welcome(0, Some([1; 16])),
            &h,
            accepts_anything
        ));
        assert!(!proves_our_launch(
            true,
            &welcome(1, Some([1; 16])),
            &h,
            accepts_anything
        ));
        assert!(!proves_our_launch(
            false,
            &welcome(1, None),
            &h,
            accepts_anything
        ));
        // Only the tokens we issued count.
        assert!(!proves_our_launch(
            false,
            &welcome(1, Some([1; 16])),
            &h,
            |_, _| false
        ));
    }

    #[tokio::test]
    async fn a_server_that_does_not_know_its_launcher_sends_no_proof() {
        // The launcher's key was forgotten (or never stored): the full pairing, both ways.
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let launch = LaunchSecret {
            key: c_id.public(),
            token: [1; 16],
        };
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let c_fut = client_launched(
            &mut cr,
            &mut cw,
            &c_id,
            "c",
            None,
            |_| false,
            |_, _| true,
            0,
        );
        let s_fut = async {
            let first = sr.recv().await?;
            server_launched(
                &mut sr,
                &mut sw,
                first,
                &s_id,
                "s",
                Some(&launch),
                |_| Admission::Pairing,
                0,
            )
            .await
        };
        let (c, _s) = tokio::join!(c_fut, s_fut);
        let c = c.unwrap();
        assert!(!c.launched);
        assert!(c.pairing.unwrap().server_must_confirm);
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        /// An honest commit and reveal.
        None,
        /// Welcome reveals a nonce other than the one ServerInfo committed to.
        WrongReveal,
        /// ServerInfo without its pair_commit.
        NoCommit,
        /// Welcome without its nonce_s.
        NoReveal,
    }

    /// A hand-driven server end. Returns what the client made of it, and the `Error` the
    /// client sent back (if any).
    async fn run_scripted_server(
        fault: Fault,
    ) -> (Result<Established, Ava1Error>, Option<gen::Error>) {
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let c_fut = client(&mut cr, &mut cw, &c_id, "laptop", |_| false, 0);
        let s_fut = async {
            let nonce_s = [0x5au8; 16];
            let m1: Hs1 = sr.recv().await.unwrap().decode().unwrap();
            let mut hs = Handshake::responder(&s_id).unwrap();
            hs.read(&m1.noise).unwrap();
            let mut si = ServerInfo {
                version: gen::PROTOCOL_VERSION,
                caps: 0,
                session_id: [3; 16],
                pair_commit: keys::pair_commit(&nonce_s),
                name: None,
            }
            .to_bytes()
            .unwrap();
            if fault == Fault::NoCommit {
                si.truncate(2 + 8 + 16); // version, caps, session_id: the old layout
            }
            sw.send_msg(
                0,
                &Hs2 {
                    noise: hs.write(&si).unwrap(),
                },
            )
            .await
            .unwrap();
            if fault == Fault::NoCommit {
                // The client has not keyed anything yet: its refusal is unsealed.
                return match sr.recv().await {
                    Ok(f) if f.ty == gen::Error::TYPE => f.decode::<gen::Error>().ok(),
                    _ => None,
                };
            }
            let m3: Hs3 = sr.recv().await.unwrap().decode().unwrap();
            hs.read(&m3.noise).unwrap();
            let k = hs.finish();
            sw.set_key(keys::control_key(&k.s2c));
            sr.set_key(keys::control_key(&k.c2s));
            let mut wl = Welcome {
                knows_you: 0,
                nonce_s: if fault == Fault::WrongReveal {
                    [0x5b; 16]
                } else {
                    nonce_s
                },
                launch_proof: None,
            }
            .to_bytes()
            .unwrap();
            if fault == Fault::NoReveal {
                wl.truncate(1); // knows_you only: the old layout
            }
            sw.send(Welcome::TYPE, 0, &wl).await.unwrap();
            if fault == Fault::None {
                return None; // the client is done; there is nothing to wait for
            }
            match sr.recv().await {
                Ok(f) if f.ty == gen::Error::TYPE => f.decode::<gen::Error>().ok(),
                _ => None,
            }
        };
        tokio::join!(c_fut, s_fut)
    }

    #[tokio::test]
    async fn the_scripted_server_with_no_fault_pairs() {
        // Guards the helper: the failures below are the fault, not the harness.
        let (c, _) = run_scripted_server(Fault::None).await;
        assert!(c.unwrap().pairing.is_some());
    }

    #[tokio::test]
    async fn a_reveal_that_does_not_match_the_commitment_aborts_before_any_code() {
        let (c, seen) = run_scripted_server(Fault::WrongReveal).await;
        assert!(
            matches!(c, Err(Ava1Error::PairingCommitMismatch)),
            "no Established, so no code was ever produced: {:?}",
            c.err()
        );
        assert_eq!(seen.unwrap().code, gen::ERR_PROTOCOL);
    }

    #[tokio::test]
    async fn a_server_that_omits_its_commitment_is_refused_with_err_protocol() {
        let (c, seen) = run_scripted_server(Fault::NoCommit).await;
        assert!(matches!(c, Err(Ava1Error::Decode(_))), "{:?}", c.err());
        assert_eq!(seen.unwrap().code, gen::ERR_PROTOCOL);
    }

    #[tokio::test]
    async fn a_welcome_without_the_reveal_is_refused_with_err_protocol() {
        let (c, seen) = run_scripted_server(Fault::NoReveal).await;
        assert!(matches!(c, Err(Ava1Error::Decode(_))), "{:?}", c.err());
        assert_eq!(seen.unwrap().code, gen::ERR_PROTOCOL);
    }

    /// A client that sends message 3 with `ci_bytes` as its ClientInfo; returns the
    /// server's result and the sealed frame the client got back.
    async fn run_scripted_client(
        ci_bytes: Vec<u8>,
    ) -> (Result<Established, Ava1Error>, Option<gen::Error>) {
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let c_fut = async {
            let mut hs = Handshake::initiator(&c_id).unwrap();
            let hello = HelloInfo {
                version_min: 1,
                version_max: 1,
                caps: 0,
            }
            .to_bytes()
            .unwrap();
            cw.send_msg(
                0,
                &Hs1 {
                    noise: hs.write(&hello).unwrap(),
                },
            )
            .await
            .unwrap();
            let m2: Hs2 = cr.recv().await.unwrap().decode().unwrap();
            hs.read(&m2.noise).unwrap();
            cw.send_msg(
                0,
                &Hs3 {
                    noise: hs.write(&ci_bytes).unwrap(),
                },
            )
            .await
            .unwrap();
            let k = hs.finish();
            cr.set_key(keys::control_key(&k.s2c));
            match cr.recv().await {
                Ok(f) if f.ty == gen::Error::TYPE => f.decode::<gen::Error>().ok(),
                _ => None,
            }
        };
        let s_fut = async {
            let first = sr.recv().await?;
            server(
                &mut sr,
                &mut sw,
                first,
                &s_id,
                "console",
                |_| Admission::Pairing,
                0,
            )
            .await
        };
        let (seen, s) = tokio::join!(c_fut, s_fut);
        (s, seen)
    }

    #[tokio::test]
    async fn a_client_without_its_nonce_is_refused_with_err_protocol() {
        // The nonce is a base field, so an old ClientInfo (name only) or an empty one fails.
        for bytes in [
            Vec::new(),
            ClientInfo {
                nonce_c: [1; 16],
                name: Some("x".into()),
                token: None,
            }
            .to_bytes()
            .unwrap()[16..]
                .to_vec(),
        ] {
            let (s, seen) = run_scripted_client(bytes).await;
            assert!(matches!(s, Err(Ava1Error::Decode(_))), "{:?}", s.err());
            assert_eq!(seen.unwrap().code, gen::ERR_PROTOCOL);
        }
    }

    #[tokio::test]
    async fn a_client_with_its_nonce_pairs() {
        let ci = ClientInfo {
            nonce_c: [1; 16],
            name: Some("x".into()),
            token: None,
        }
        .to_bytes()
        .unwrap();
        let (s, seen) = run_scripted_client(ci).await;
        assert!(s.unwrap().pairing.is_some());
        assert!(seen.is_none());
    }

    #[tokio::test]
    async fn paired_devices_agree_on_keys_peers_and_names() {
        let e = run(true, true, false).await;
        let (c, s) = (e.c.unwrap(), e.s.unwrap());
        assert_eq!(
            (c.keys.c2s, c.keys.s2c, c.keys.hash),
            (s.keys.c2s, s.keys.s2c, s.keys.hash)
        );
        assert_eq!(c.session_id, s.session_id);
        assert_eq!((c.peer_key, s.peer_key), (e.s_pub, e.c_pub));
        assert_eq!(
            (c.peer_name.as_str(), s.peer_name.as_str()),
            ("console", "laptop")
        );
        assert_eq!((c.pairing, s.pairing), (None, None));
    }

    #[tokio::test]
    async fn an_unknown_client_must_be_confirmed_while_the_window_is_open() {
        let e = run(false, false, true).await;
        let (cp, sp) = (e.c.unwrap().pairing.unwrap(), e.s.unwrap().pairing.unwrap());
        assert!(cp.server_must_confirm && sp.server_must_confirm);
    }

    #[tokio::test]
    async fn an_unknown_client_is_refused_when_pairing_is_closed() {
        let e = run(false, true, false).await;
        assert!(
            matches!(e.c, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_PAIRING_CLOSED),
            "{:?}",
            e.c.err()
        );
        assert!(matches!(e.s, Err(Ava1Error::NotPaired)));
    }

    #[tokio::test]
    async fn a_client_that_does_not_know_the_server_confirms_locally_only() {
        let e = run(true, false, false).await;
        assert!(!e.c.unwrap().pairing.unwrap().server_must_confirm);
        assert_eq!(e.s.unwrap().pairing, None);
    }

    #[tokio::test]
    async fn a_version_the_server_cannot_speak_is_refused() {
        let s_id = Identity::generate().unwrap();
        let c_id = Identity::generate().unwrap();
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let mut hs = Handshake::initiator(&c_id).unwrap();
        let hello = HelloInfo {
            version_min: 2,
            version_max: 2,
            caps: 0,
        }
        .to_bytes()
        .unwrap();
        cw.send_msg(
            0,
            &Hs1 {
                noise: hs.write(&hello).unwrap(),
            },
        )
        .await
        .unwrap();
        let first = sr.recv().await.unwrap();
        let s = server(
            &mut sr,
            &mut sw,
            first,
            &s_id,
            "console",
            |_| Admission::Known,
            0,
        )
        .await;
        assert!(matches!(
            s,
            Err(Ava1Error::Version {
                min: 2,
                max: 2,
                ours: 1
            })
        ));
        let e: gen::Error = cr.recv().await.unwrap().decode().unwrap();
        assert_eq!(e.code, gen::ERR_UNSUPPORTED_VERSION);
    }

    #[tokio::test]
    async fn after_the_handshake_frames_are_sealed_and_replays_fail() {
        let (c_id, s_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let ((mut cr, mut cw), (mut sr, mut sw)) = pipe();
        let s_task = async {
            let first = sr.recv().await.unwrap();
            server(&mut sr, &mut sw, first, &s_id, "s", |_| Admission::Known, 0)
                .await
                .unwrap();
            sr
        };
        let c_task = async {
            client(&mut cr, &mut cw, &c_id, "c", |_| true, 0)
                .await
                .unwrap()
        };
        let (est, mut sr) = tokio::join!(c_task, s_task);
        cw.send_msg(0, &gen::Ping { seq: 1, t_us: 1 })
            .await
            .unwrap();
        let f = sr.recv().await.unwrap();
        assert_eq!(f.ty, gen::Ping::TYPE);
        assert_eq!(
            f.flags & crate::frame::FLAG_SEALED,
            crate::frame::FLAG_SEALED
        );
        // Replay: resetting the key restarts the counter at 0, which the receiver has passed.
        cw.set_key(keys::control_key(&est.keys.c2s));
        cw.send_msg(0, &gen::Ping { seq: 2, t_us: 2 })
            .await
            .unwrap();
        assert!(matches!(sr.recv().await, Err(Ava1Error::BadTag)));
    }

    #[tokio::test]
    async fn a_frame_sealed_with_the_wrong_key_is_rejected() {
        let ((_cr, mut cw), (mut sr, _sw)) = pipe();
        cw.set_key([1; 32]);
        sr.set_key([2; 32]);
        cw.send_msg(0, &gen::Ping { seq: 1, t_us: 1 })
            .await
            .unwrap();
        assert!(matches!(sr.recv().await, Err(Ava1Error::BadTag)));
    }

    #[tokio::test]
    async fn an_unsealed_frame_after_keying_is_rejected() {
        let ((_cr, mut cw), (mut sr, _sw)) = pipe();
        sr.set_key([2; 32]);
        cw.send_msg(0, &gen::Ping { seq: 1, t_us: 1 })
            .await
            .unwrap();
        assert!(matches!(sr.recv().await, Err(Ava1Error::BadTag)));
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_before_it_is_read() {
        let ((_cr, mut cw), (mut sr, _sw)) = pipe();
        sr.set_max_body(1024);
        cw.send(gen::Ping::TYPE, 0, &vec![0u8; 2048]).await.unwrap();
        assert!(matches!(
            sr.recv().await,
            Err(Ava1Error::Header(crate::frame::HeaderError::TooLong(2048)))
        ));
    }

    #[tokio::test]
    async fn the_ignorable_flag_reaches_the_receiver() {
        let ((_cr, mut cw), (mut sr, _sw)) = pipe();
        cw.send_ignorable(0x5e, 0, b"future").await.unwrap();
        let f = sr.recv().await.unwrap();
        assert!(f.ignorable());
        assert_eq!(f.body, b"future");
    }
}
