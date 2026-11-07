//! Devices: the verbs `device_pair`, `device_list`, `device_revoke` and `api_status`, and the two
//! calls the network API makes into them, `redeem_code` and `device_for_token` (ADR-0004,
//! ADR-0005, ADR-0009, ADR-0012).
//!
//! A device is a controller entity: an ID, a name, the SHA-256 of its token and its last-seen
//! telemetry. Pairing and revoking are controller commands that take the path of `commands.rs`. A
//! seat cannot pair or revoke: the CLI refuses under `PF_SEAT_TOKEN`, and the controller checks the
//! caller of the socket again, as it does for `claim` (ADR-0005).
//!
//! The pairing code lives in memory only, with its expiry. At most one code is live. A restart
//! voids it. No token, code or key is ever written to a log, the ledger or an error.

use std::net::IpAddr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use pf_api::Refusal;
use pf_api::code::{PairingCode, SECRET_BYTES, hex, unhex};
use pf_api::types::{
    DeviceList, DevicePairing, DeviceRecord, DeviceRevokeArgs, DeviceRevoked, PairResult,
    SCHEMA_VERSION, check_device_name,
};
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{DeviceRow, Effect, NEW_ID, NewDevice, Store};
use pf_ipc::Response;
use serde::Serialize;
use serde_json::Value;

use super::api::LiveCode;
use super::commands::{Plan, check_command_id, field, now};
use super::seat::random_bytes;
use super::{Controller, lock};

/// A device ID names a stored row, so it is short. A longer text is no ID.
const DEVICE_ID_MAX_CHARS: usize = 64;

/// The bytes of a device token. The token is these bytes as 64 hex digits.
const TOKEN_BYTES: usize = 32;

/// The content of a `device_revoke` admission.
#[derive(Serialize)]
struct RevokeContent<'a> {
    id: &'a str,
}

/// The content of a `device_pair` admission: the name. The code's secret is the command ID's
/// source, and the token is a fresh random value, so neither is content.
#[derive(Serialize)]
struct PairContent<'a> {
    device_name: &'a str,
}

fn record(d: DeviceRow) -> DeviceRecord {
    DeviceRecord {
        id: d.id,
        name: d.name,
        paired_at: d.paired_at,
        last_seen_at: d.last_seen_at,
        last_peer: d.last_peer,
        revoked_at: d.revoked_at,
    }
}

/// Compare two secrets without stopping at the first byte that differs.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Controller {
    /// `device_pair`: make one code, which holds the API address, a single-use secret and the SPKI
    /// hash of the server key. It replaces the live code.
    pub(super) fn verb_device_pair(&self) -> Response {
        let (Some(key), Some(listen)) = (self.api.key.as_ref(), self.api.advertised()) else {
            return Response::err(match self.api.state().error {
                Some(why) => format!("the API cannot start: {why}"),
                None => "the API is off: set PF_API_LISTEN (or pass --api-listen) and restart the \
                         controller"
                    .to_string(),
            });
        };
        let secret = match random_bytes(&self.cfg.random_device, SECRET_BYTES) {
            Ok(bytes) => <[u8; SECRET_BYTES]>::try_from(bytes.as_slice()).ok(),
            Err(why) => return Response::err(format!("no code can be made: {why}")),
        };
        let Some(secret) = secret else {
            return Response::err("no code can be made: the random source gave too few bytes");
        };
        let code = PairingCode {
            secret,
            spki_sha256: key.spki_sha256(),
            listen: listen.to_string(),
        };
        let ttl = self.cfg.pair_ttl;
        *lock(&self.api.pairing) = Some(LiveCode {
            secret,
            expires: Instant::now() + ttl,
        });
        let at = SystemTime::now()
            .checked_add(ttl)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        Response::ok(
            serde_json::to_value(DevicePairing {
                schema_version: SCHEMA_VERSION,
                code: code.encode(),
                expires_at: pf_engine::engine::format_utc(at.as_secs(), at.subsec_millis()),
            })
            .unwrap_or(Value::Null),
        )
    }

    /// `device_list`: every device, live ones first.
    pub(super) fn verb_device_list(&self) -> Response {
        match self.store.devices() {
            Ok(rows) => Response::ok(
                serde_json::to_value(DeviceList {
                    schema_version: SCHEMA_VERSION,
                    devices: rows.into_iter().map(record).collect(),
                })
                .unwrap_or(Value::Null),
            ),
            Err(e) => Response::err(e.to_string()),
        }
    }

    /// `device_revoke`: the device's next call is refused at once, because the API reads the store
    /// on each call.
    pub(super) fn verb_device_revoke(&self, args: &Value) -> Response {
        let args: DeviceRevokeArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        let n = args.id.chars().count();
        if n == 0 || n > DEVICE_ID_MAX_CHARS {
            return Response::err(format!(
                "a device ID is 1 to {DEVICE_ID_MAX_CHARS} characters"
            ));
        }
        let content = serde_json::to_string(&RevokeContent { id: &args.id }).unwrap_or_default();
        // The store knows whether the ID names a live device, and its answer decides.
        let plan = Plan::Admit {
            effects: vec![Effect::RevokeDevice {
                id: args.id.clone(),
                revoked_at: now(),
            }],
            subject: args.id.clone(),
            line: format!(
                "device_revoke {} admitted command={}",
                field(&args.id),
                args.command_id
            ),
        };
        match self.run_command("device_revoke", &args.command_id, content, plan) {
            Ok((admission, replayed)) => {
                let Some(id) = &admission.unit_id else {
                    return Response::err("the admission names no device");
                };
                match self.store.devices() {
                    Ok(rows) => match rows.into_iter().find(|d| &d.id == id) {
                        Some(row) => Response::ok(
                            serde_json::to_value(DeviceRevoked {
                                schema_version: SCHEMA_VERSION,
                                replayed,
                                device: record(row),
                            })
                            .unwrap_or(Value::Null),
                        ),
                        None => Response::err(format!("device {id} is not in the store")),
                    },
                    Err(e) => Response::err(e.to_string()),
                }
            }
            Err(why) => Response::err(why),
        }
    }

    /// `api_status`: off, listening, waiting or failed, and the key's hash when the key exists.
    pub(super) fn verb_api_status(&self) -> Response {
        Response::ok(serde_json::to_value(self.api.state()).unwrap_or(Value::Null))
    }

    /// Use up the live code if `secret_hex` is its secret, and pair a device. The code is used up
    /// before the answer, so a second redeem of it is refused whatever else fails.
    pub(super) fn redeem_code(
        &self,
        secret_hex: &str,
        device_name: &str,
        peer: IpAddr,
    ) -> Result<PairResult, Refusal> {
        let secret = unhex(secret_hex)
            .filter(|s| s.len() == SECRET_BYTES)
            .ok_or(Refusal::Code)?;
        // A bad name costs the operator no code.
        check_device_name(device_name).map_err(Refusal::Name)?;
        {
            let mut live = lock(&self.api.pairing);
            match live.as_ref() {
                Some(code) if Instant::now() >= code.expires => {
                    *live = None;
                    return Err(Refusal::Code);
                }
                Some(code) if same_secret(&code.secret, &secret) => *live = None,
                // A wrong secret leaves the live code alone: a guess must not burn it.
                _ => return Err(Refusal::Code),
            }
        }
        let token_bytes = random_bytes(&self.cfg.random_device, TOKEN_BYTES)
            .map_err(|_| Refusal::Server("no token can be made".to_string()))?;
        let token = hex(&token_bytes);
        let token_sha256 = sha256_hex(&token_bytes);
        let content = serde_json::to_string(&PairContent { device_name }).unwrap_or_default();
        let plan = Plan::Admit {
            effects: vec![Effect::AddDevice(NewDevice {
                name: device_name.to_string(),
                token_sha256: token_sha256.clone(),
                paired_at: now(),
            })],
            subject: NEW_ID.to_string(),
            // The line names the device, its name and the peer. It never holds the code or the
            // token.
            line: format!(
                "device_pair {NEW_ID} admitted name={} peer={peer}",
                field(device_name)
            ),
        };
        // The command ID is the SHA-256 of the code's secret, so a redeem replays at most once.
        let command_id = sha256_hex(&secret);
        match self.run_command("device_pair", &command_id, content, plan) {
            Ok((_, false)) => {}
            // The store saw this secret before: the code was used.
            Ok((_, true)) => return Err(Refusal::Code),
            Err(why) => return Err(Refusal::Server(why)),
        }
        match self.store.device_by_token(&token_sha256) {
            Ok(Some(row)) => Ok(PairResult {
                schema_version: SCHEMA_VERSION,
                device: record(row),
                token,
            }),
            Ok(None) => Err(Refusal::Server(
                "the device was paired and is not in the store".to_string(),
            )),
            Err(e) => Err(Refusal::Server(e.to_string())),
        }
    }

    /// The live device whose token has this SHA-256. It reads the store on each call, so a revoke
    /// takes effect on the next call, and it records last-seen (telemetry, at most once a minute).
    pub(super) fn device_for_token(
        &self,
        token_sha256: &str,
        peer: IpAddr,
    ) -> Option<DeviceRecord> {
        let row = self.store.device_by_token(token_sha256).ok().flatten()?;
        let _ = self.store.touch_device(&row.id, &now(), &peer.to_string());
        Some(record(row))
    }
}
