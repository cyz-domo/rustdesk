use std::{
    sync::{
        atomic::Ordering,
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use hbb_common::{
    config::LocalConfig,
    log,
    rendezvous_proto::{ConnType, PeerInfo},
    tokio::{self, time::sleep},
    Stream,
};

use base::{
    config::keys,
    message_proto::{Hash, TestDelay, WindowsSession},
};

use crate::{
    client::{Client, Interface, LoginConfigHandler},
    ui_session_interface::{InvokeUiSession, Session},
};

const FIRST_PROBE_DELAY_SECS: u64 = 30;
const MAX_PROBE_DELAY_SECS: u64 = 300;
const PROBE_DEADLINE_SECS: u64 = 2 * 60 * 60;
// Hard cap on relay->direct upgrade reconnects per session, so a punch that probes direct but
// keeps winning the race as relay cannot loop the user through endless 1-2s black screens.
const MAX_UPGRADE_RECONNECTS: usize = 1;

#[derive(Clone)]
struct ProbeInterface {
    lch: Arc<RwLock<LoginConfigHandler>>,
}

#[hbb_common::tokio::async_trait]
impl Interface for ProbeInterface {
    fn send(&self, _data: crate::client::Data) {}
    fn msgbox(&self, _msgtype: &str, _title: &str, _text: &str, _link: &str) {}
    fn handle_login_error(&self, _err: &str) -> bool {
        false
    }
    fn handle_peer_info(&self, _pi: PeerInfo) {}
    fn set_multiple_windows_session(&self, _sessions: Vec<WindowsSession>) {}
    fn on_error(&self, _err: &str) {}
    async fn handle_hash(&self, _pass: &str, _hash: Hash, _peer: &mut Stream) -> bool {
        false
    }
    async fn handle_login_from_ui(
        &self,
        _os_username: String,
        _os_password: String,
        _password: String,
        _remember: bool,
        _peer: &mut Stream,
    ) {
    }
    async fn handle_test_delay(&self, t: TestDelay, peer: &mut Stream) {
        if !t.from_client {
            crate::client::handle_test_delay(t, peer).await;
        }
    }
    fn get_lch(&self) -> Arc<RwLock<LoginConfigHandler>> {
        self.lch.clone()
    }
    fn on_establish_connection_error(&self, _err: String) {}
}

/// First probe delay in seconds, from the `upgrade-probe-interval` option (seconds, like
/// `relay-fallback-delay`); anything unparseable keeps the default.
fn probe_interval_secs() -> u64 {
    parse_probe_interval(&LocalConfig::get_option(
        keys::OPTION_UPGRADE_PROBE_INTERVAL,
    ))
}

fn parse_probe_interval(raw: &str) -> u64 {
    match raw.trim().parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs > 0.0 => (secs.round() as u64).max(1),
        _ => FIRST_PROBE_DELAY_SECS,
    }
}

/// While the live connection is relayed, periodically re-race the punching machinery in the
/// background; the NAT mappings both sides hold may become punchable later in the session.
/// On a probe that reaches the peer directly, `Client::start` has already cleared the shared
/// `direct_failures`, so the follow-up reconnect gets the full punch window and upgrades the
/// session to direct at the cost of a brief black screen.
pub fn spawn_direct_upgrade_probe<T: InvokeUiSession>(
    handler: &Session<T>,
    key: &str,
    token: &str,
    round: u32,
) {
    if !crate::common::get_upgrade_to_direct_enabled() {
        return;
    }
    if relay_is_forced(&handler)
        || handler.upgrade_attempts.load(Ordering::SeqCst) >= MAX_UPGRADE_RECONNECTS
    {
        return;
    }
    let handler = handler.clone();
    let key = key.to_owned();
    let token = token.to_owned();
    // The task lives on the current connection round's runtime: when this io_loop returns (the
    // upgrade reconnect itself, or any disconnect) the runtime is dropped and the probe dies.
    // `round_is_live` covers the gap where the old io_loop has not returned yet.
    tokio::spawn(async move {
        run_probe(handler, key, token, round).await;
    });
}

/// The probe belongs to `round`: once that round was replaced (any reconnect) or the session
/// ended, upgrading would revive a closed session or stack extra rounds.
fn round_is_live<T: InvokeUiSession>(handler: &Session<T>, round: u32) -> bool {
    let crs = handler.connection_round_state.lock().unwrap();
    crs.is_connected() && !crs.is_round_gt(round)
}

/// Relaying chosen for this session can be retried once punching succeeds; relaying the
/// *server policy* demands, or that the user pinned for this peer, must be respected.
fn relay_is_forced<T: InvokeUiSession>(handler: &Session<T>) -> bool {
    let lc = handler.lc.read().unwrap();
    lc.force_relay || lc.policy_relay || lc.peer_relay
}

async fn run_probe<T: InvokeUiSession>(
    handler: Session<T>,
    key: String,
    token: String,
    round: u32,
) {
    let interval = probe_interval_secs();
    let max_delay = interval.max(MAX_PROBE_DELAY_SECS);
    let mut delay = interval;
    let started = Instant::now();
    loop {
        sleep(Duration::from_secs(delay)).await;
        if !round_is_live(&handler, round) {
            return;
        }
        if started.elapsed() >= Duration::from_secs(PROBE_DEADLINE_SECS) {
            log::info!("direct upgrade probe stopped: deadline reached");
            return;
        }
        if relay_is_forced(&handler)
            || handler.upgrade_attempts.load(Ordering::SeqCst) >= MAX_UPGRADE_RECONNECTS
        {
            return;
        }
        let probe_lch = {
            let lc = handler.lc.read().unwrap();
            Arc::new(RwLock::new(lc.clone_for_probe()))
        };
        let probe_iface = ProbeInterface { lch: probe_lch };
        let id = handler.get_id();
        let result = Client::start(&id, &key, &token, ConnType::default(), probe_iface).await;
        match result {
            Ok(((_stream, true, _pk, _kcp, _typ), _feedback)) => {
                // The probe itself advanced nothing on the live session, but another reconnect
                // may have landed while it ran; only upgrade while our round is still current.
                if !round_is_live(&handler, round) {
                    return;
                }
                let attempts = handler.upgrade_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                log::info!("direct path available, prompting relay-to-direct upgrade (attempt {attempts})");
                handler.lc.write().unwrap().set_direct_failure(0);
                handler.ui_handler.msgbox(
                    "upgrade-direct",
                    "Direct connection available",
                    "Direct connection is now available. Do you want to upgrade to direct connection now?",
                    "",
                    false,
                );
                return;
            }
            Ok(((_stream, false, _pk, _kcp, _typ), _feedback)) => {
                log::debug!("direct upgrade probe landed on relay again");
            }
            Err(err) => {
                log::debug!("direct upgrade probe failed: {err}");
            }
        }
        delay = (delay * 2).min(max_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_probe_interval, FIRST_PROBE_DELAY_SECS};

    #[test]
    fn probe_interval_falls_back_to_default_on_anything_unusable() {
        for raw in ["", "  ", "abc", "0", "-5", "nan", "inf"] {
            assert_eq!(parse_probe_interval(raw), FIRST_PROBE_DELAY_SECS, "{raw:?}");
        }
    }

    #[test]
    fn probe_interval_accepts_seconds_with_rounding() {
        assert_eq!(parse_probe_interval("45"), 45);
        assert_eq!(parse_probe_interval(" 60.4 "), 60);
        assert_eq!(parse_probe_interval("0.4"), 1);
    }
}
