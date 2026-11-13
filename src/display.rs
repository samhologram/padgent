//! Bounded local display leases and declarative scenes shared by HTTP and MCP.
use crate::colour::Rgb;
use crate::model::{Activity, KEY_COUNT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayCommand {
    Claim {
        owner: String,
        keys: Vec<usize>,
        #[serde(default)]
        ttl_seconds: Option<f64>,
    },
    Renew {
        token: String,
        #[serde(default)]
        ttl_seconds: Option<f64>,
    },
    Release {
        token: String,
    },
    Set {
        token: String,
        keys: BTreeMap<String, Option<Scene>>,
    },
    Clear {
        token: String,
        #[serde(default)]
        keys: Option<Vec<usize>>,
    },
    Events {
        token: String,
    },
    State {},
}
#[derive(Debug)]
pub struct DisplayError {
    pub status: u16,
    pub message: String,
}
impl From<(u16, String)> for DisplayError {
    fn from((status, message): (u16, String)) -> Self {
        Self { status, message }
    }
}
use std::io::Read;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layer {
    pub kind: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: String,
    pub text: String,
    pub size: f32,
}
impl Default for Layer {
    fn default() -> Self {
        Self {
            kind: "rect".into(),
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.2,
            color: "#ffffff".into(),
            text: String::new(),
            size: 0.12,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scene {
    pub kind: String,
    pub title: String,
    pub text: String,
    pub activity: String,
    pub hue: f32,
    pub style: String,
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub animation: String,
    pub period_seconds: f32,
    pub layers: Vec<Layer>,
}
impl Default for Scene {
    fn default() -> Self {
        Self {
            kind: "custom".into(),
            title: String::new(),
            text: String::new(),
            activity: "idle".into(),
            hue: 270.0,
            style: "orb".into(),
            background: "#10131e".into(),
            foreground: "#ffffff".into(),
            accent: "#856be0".into(),
            animation: "none".into(),
            period_seconds: 4.0,
            layers: Vec::new(),
        }
    }
}
impl Scene {
    fn validate(&self) -> Result<(), String> {
        if !["native", "custom"].contains(&self.kind.as_str()) {
            return Err("kind must be native or custom".into());
        }
        if self.title.chars().count() > 64 || self.text.chars().count() > 160 {
            return Err("text is too long".into());
        }
        if Activity::parse(&self.activity).is_none()
            || !self.hue.is_finite()
            || !(0.0..=360.0).contains(&self.hue)
            || !["orb", "dots"].contains(&self.style.as_str())
        {
            return Err("invalid native pattern".into());
        }
        if [
            self.background.as_str(),
            self.foreground.as_str(),
            self.accent.as_str(),
        ]
        .iter()
        .any(|c| Rgb::parse(c).is_none())
        {
            return Err("colors must be hex RGB".into());
        }
        if !["none", "pulse", "drift", "gradient"].contains(&self.animation.as_str())
            || !self.period_seconds.is_finite()
            || !(1.0..=120.0).contains(&self.period_seconds)
        {
            return Err("invalid animation or period".into());
        }
        if self.layers.len() > 16 {
            return Err("at most 16 layers".into());
        }
        for l in &self.layers {
            if !["rect", "circle", "ellipse", "line", "text"].contains(&l.kind.as_str())
                || Rgb::parse(&l.color).is_none()
                || l.text.chars().count() > 100
                || [l.x, l.y, l.width, l.height, l.size]
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            {
                return Err("invalid scene layer".into());
            }
        }
        Ok(())
    }
    pub fn animated(&self) -> bool {
        self.animation != "none"
            || (self.kind == "native"
                && Activity::parse(&self.activity).is_some_and(Activity::animated))
    }
}

#[derive(Clone)]
struct Lease {
    token: String,
    owner: String,
    keys: Vec<usize>,
    expires_at: f64,
    generations: [u64; KEY_COUNT],
    scenes: [Option<Scene>; KEY_COUNT],
    events: VecDeque<Value>,
}
#[derive(Default)]
pub struct DisplayController {
    leases: Vec<Lease>,
    generation: u64,
}
impl DisplayController {
    pub fn prune(&mut self, now: f64) -> bool {
        let n = self.leases.len();
        self.leases.retain(|l| l.expires_at > now);
        n != self.leases.len()
    }
    pub fn summary(&self, now: f64) -> Option<String> {
        let owners: Vec<_> = self
            .leases
            .iter()
            .filter(|l| l.expires_at > now)
            .map(|l| l.owner.as_str())
            .collect();
        (!owners.is_empty()).then(|| format!("External control: {}", owners.join(", ")))
    }
    pub fn clear(&mut self) {
        self.leases.clear();
    }
    pub fn scene(&self, key: usize, now: f64) -> Option<(Scene, u64)> {
        self.leases
            .iter()
            .find(|l| l.expires_at > now && l.keys.contains(&key))
            .and_then(|l| l.scenes[key].clone().map(|s| (s, l.generations[key])))
    }
    pub fn press(&mut self, key: usize, generation: u64, now: f64) {
        if let Some(l) = self.leases.iter_mut().find(|l| {
            l.expires_at > now && l.keys.contains(&key) && l.generations[key] == generation
        }) {
            if l.events.len() == 128 {
                l.events.pop_front();
            }
            l.events
                .push_back(json!({"key":key,"type":"press","at":now}));
        }
    }
    pub fn execute(&mut self, command: DisplayCommand, now: f64) -> Result<Value, DisplayError> {
        self.prune(now);
        let err = |message: &str| DisplayError {
            status: 400,
            message: message.to_string(),
        };
        match command {
            DisplayCommand::State {} => Ok(
                json!({"leases":self.leases.iter().map(|l| json!({"owner":l.owner,"keys":l.keys,"expires_at":l.expires_at})).collect::<Vec<_>>() }),
            ),
            DisplayCommand::Claim {
                owner,
                keys,
                ttl_seconds,
            } => {
                if owner.trim().is_empty() || owner.chars().count() > 80 {
                    return Err(err("owner is required, maximum 80 characters"));
                }
                if keys.is_empty() || keys.len() > KEY_COUNT || keys.iter().any(|k| *k >= KEY_COUNT)
                {
                    return Err(err("keys must contain indices 0 through 8"));
                }
                let mut unique = keys.clone();
                unique.sort_unstable();
                unique.dedup();
                if unique.len() != keys.len() {
                    return Err(err("duplicate key"));
                }
                let duration = ttl(ttl_seconds)?;
                if self
                    .leases
                    .iter()
                    .any(|l| l.keys.iter().any(|k| keys.contains(k)))
                {
                    return Err(DisplayError {
                        status: 409,
                        message: "key already claimed".into(),
                    });
                }
                let mut random = [0u8; 32];
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut f| f.read_exact(&mut random))
                    .map_err(|_| DisplayError {
                        status: 500,
                        message: "random source unavailable".into(),
                    })?;
                let token = random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                self.generation = self.generation.wrapping_add(1);
                let scenes = std::array::from_fn(|i| keys.contains(&i).then(Scene::default));
                self.leases.push(Lease {
                    token: token.clone(),
                    owner,
                    keys,
                    expires_at: now + duration,
                    generations: [self.generation; KEY_COUNT],
                    scenes,
                    events: VecDeque::new(),
                });
                Ok(json!({"token":token,"expires_at":now+duration}))
            }
            DisplayCommand::Renew { token, ttl_seconds } => {
                let duration = ttl(ttl_seconds)?;
                let lease = self.lease_mut(&token)?;
                lease.expires_at = now + duration;
                Ok(json!({"expires_at":now+duration}))
            }
            DisplayCommand::Release { token } => {
                let i = self
                    .leases
                    .iter()
                    .position(|l| l.token == token)
                    .ok_or_else(|| DisplayError {
                        status: 403,
                        message: "invalid or expired lease".into(),
                    })?;
                self.leases.remove(i);
                Ok(json!({"ok":true}))
            }
            DisplayCommand::Events { token } => {
                let events: Vec<_> = self.lease_mut(&token)?.events.drain(..).collect();
                Ok(json!({"events":events}))
            }
            DisplayCommand::Clear { token, keys } => {
                let owned = self.lease_mut(&token)?.keys.clone();
                let keys = keys.unwrap_or(owned);
                if keys.is_empty() || keys.len() > KEY_COUNT {
                    return Err(err("keys must contain 1 to 9 entries"));
                }
                let mut unique = keys.clone();
                unique.sort_unstable();
                unique.dedup();
                if unique.len() != keys.len() {
                    return Err(err("duplicate key"));
                }
                let keys = keys
                    .into_iter()
                    .map(|key| (key.to_string(), None))
                    .collect();
                self.execute(DisplayCommand::Set { token, keys }, now)
            }
            DisplayCommand::Set { token, keys } => {
                if keys.is_empty() || keys.len() > KEY_COUNT {
                    return Err(err("keys must contain 1 to 9 entries"));
                }
                let lease = self.lease_mut(&token)?;
                let mut updates = Vec::new();
                for (key, scene) in keys {
                    let k = key
                        .as_bytes()
                        .first()
                        .copied()
                        .filter(|_| key.len() == 1)
                        .and_then(|b| b.checked_sub(b'0'))
                        .filter(|k| (*k as usize) < KEY_COUNT)
                        .map(usize::from)
                        .filter(|k| lease.keys.contains(k))
                        .ok_or_else(|| err("key is not in lease"))?;
                    if let Some(scene) = &scene {
                        scene.validate().map_err(|e| err(&e))?;
                    }
                    updates.push((k, scene.unwrap_or_default()));
                }
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                let lease = self.lease_mut(&token)?;
                for (key, scene) in updates {
                    lease.scenes[key] = Some(scene);
                    lease.generations[key] = generation;
                }
                Ok(json!({"ok":true}))
            }
        }
    }
    fn lease_mut(&mut self, token: &str) -> Result<&mut Lease, DisplayError> {
        self.leases
            .iter_mut()
            .find(|l| l.token == token)
            .ok_or_else(|| DisplayError {
                status: 403,
                message: "invalid or expired lease".into(),
            })
    }
}
fn ttl(value: Option<f64>) -> Result<f64, DisplayError> {
    let n = value.unwrap_or(60.0);
    if !n.is_finite() || !(1.0..=3600.0).contains(&n) {
        Err(DisplayError {
            status: 400,
            message: "ttl_seconds must be 1 through 3600".into(),
        })
    } else {
        Ok(n)
    }
}

impl Scene {
    pub fn native_face(&self) -> crate::model::SessionFace {
        let colour = crate::colour::oklch(0.75, 0.16, self.hue);
        crate::model::SessionFace {
            key: String::new(),
            opacity: 1.0,
            badge: String::new(),
            agent_name: "External".into(),
            title: self.title.clone(),
            text: self.text.clone(),
            activity: Activity::parse(&self.activity).unwrap_or(Activity::Idle),
            tool: None,
            orb: colour,
            glow: colour,
            since: 0.0,
            subagents: 0,
            dots: self.style == "dots",
            unread: 0,
            scheduled: false,
            last_message: self.text.clone(),
            last_turn: crate::model::now(),
            agent_colour: colour,
        }
    }
    pub fn sample(&self, x: f32, y: f32, t: f32) -> Rgb {
        let bg = Rgb::parse(&self.background).unwrap_or_default();
        let accent = Rgb::parse(&self.accent).unwrap_or_default();
        let phase = t * std::f32::consts::TAU / self.period_seconds;
        let amount = match self.animation.as_str() {
            "pulse" => 0.14 + 0.12 * (0.5 + 0.5 * phase.sin()),
            "drift" => {
                (1.0 - ((x - 0.5 - 0.25 * phase.sin()).powi(2) + (y - 0.5).powi(2)).sqrt()).max(0.0)
                    * 0.35
            }
            "gradient" => (y * 0.45 + 0.1 * phase.sin()).clamp(0.0, 0.55),
            _ => y * 0.24,
        };
        bg.mix(accent, amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(
        controller: &mut DisplayController,
        now: f64,
        keys: Vec<usize>,
        ttl_seconds: f64,
    ) -> String {
        controller
            .execute(
                DisplayCommand::Claim {
                    owner: "test-client".into(),
                    keys,
                    ttl_seconds: Some(ttl_seconds),
                },
                now,
            )
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn lease_claim_is_exclusive_authenticated_and_expires() {
        let mut controller = DisplayController::default();
        let token = claim(&mut controller, 100.0, vec![0, 1], 5.0);
        let conflict = controller
            .execute(
                DisplayCommand::Claim {
                    owner: "other".into(),
                    keys: vec![1],
                    ttl_seconds: None,
                },
                101.0,
            )
            .unwrap_err();
        assert_eq!(conflict.status, 409);

        let invalid = controller
            .execute(
                DisplayCommand::Set {
                    token: "wrong-token".into(),
                    keys: [("0".into(), Some(Scene::default()))].into(),
                },
                101.0,
            )
            .unwrap_err();
        assert_eq!(invalid.status, 403);
        assert_eq!(
            controller.scene(0, 101.0).unwrap().0,
            Scene::default(),
            "failed authentication must not alter the claimed default scene"
        );

        let state = controller.execute(DisplayCommand::State {}, 101.0).unwrap();
        assert_eq!(state["leases"].as_array().unwrap().len(), 1);
        assert!(
            state["leases"][0].get("token").is_none(),
            "read-only state must not disclose lease credentials"
        );
        assert!(controller.scene(0, 104.9).is_some());
        assert!(
            controller.scene(0, 105.0).is_none(),
            "scene is inactive at expiry"
        );
        assert!(controller.prune(105.0));
        assert_eq!(
            controller.execute(DisplayCommand::State {}, 105.0).unwrap()["leases"],
            json!([])
        );
        assert_eq!(token.len(), 64);
    }

    #[test]
    fn invalid_scene_batch_does_not_partially_replace_existing_scenes() {
        let mut controller = DisplayController::default();
        let token = claim(&mut controller, 10.0, vec![0, 1], 60.0);
        let original = Scene {
            title: "kept".into(),
            ..Scene::default()
        };
        controller
            .execute(
                DisplayCommand::Set {
                    token: token.clone(),
                    keys: [("0".into(), Some(original.clone()))].into(),
                },
                11.0,
            )
            .unwrap();

        let invalid_layer = Scene {
            layers: vec![Layer {
                kind: "text".into(),
                x: 1.1,
                y: 0.5,
                width: 0.2,
                height: 0.2,
                size: 0.1,
                color: "#ffffff".into(),
                text: "bad".into(),
            }],
            ..Scene::default()
        };
        let error = controller
            .execute(
                DisplayCommand::Set {
                    token,
                    keys: [
                        (
                            "0".into(),
                            Some(Scene {
                                title: "must not apply".into(),
                                ..Scene::default()
                            }),
                        ),
                        ("1".into(), Some(invalid_layer)),
                    ]
                    .into(),
                },
                12.0,
            )
            .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(
            controller.scene(0, 12.0).unwrap().0,
            original,
            "valid prefix of a rejected batch must remain unchanged"
        );
        assert_eq!(controller.scene(1, 12.0).unwrap().0, Scene::default());

        for invalid in [
            Scene {
                title: "x".repeat(65),
                ..Scene::default()
            },
            Scene {
                text: "x".repeat(161),
                ..Scene::default()
            },
            Scene {
                period_seconds: f32::NAN,
                ..Scene::default()
            },
        ] {
            assert!(
                invalid.validate().is_err(),
                "out-of-bounds text and non-finite animation values are rejected"
            );
        }
        let bad_layer = Scene {
            layers: vec![Layer {
                x: 1.01,
                ..Layer::default()
            }],
            ..Scene::default()
        };
        assert!(
            bad_layer.validate().is_err(),
            "layer coordinates must stay normalized"
        );
    }

    #[test]
    fn clear_blanks_owned_subset_or_all_without_changing_lease_ownership() {
        let mut controller = DisplayController::default();
        let token = claim(&mut controller, 10.0, vec![0, 1], 60.0);
        let other_token = controller
            .execute(
                DisplayCommand::Claim {
                    owner: "other-client".into(),
                    keys: vec![2],
                    ttl_seconds: Some(60.0),
                },
                10.0,
            )
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        let first = Scene {
            title: "first".into(),
            ..Scene::default()
        };
        let second = Scene {
            title: "second".into(),
            ..Scene::default()
        };
        let other = Scene {
            title: "other owner".into(),
            ..Scene::default()
        };
        controller
            .execute(
                DisplayCommand::Set {
                    token: token.clone(),
                    keys: [
                        ("0".into(), Some(first)),
                        ("1".into(), Some(second.clone())),
                    ]
                    .into(),
                },
                11.0,
            )
            .unwrap();
        controller
            .execute(
                DisplayCommand::Set {
                    token: other_token,
                    keys: [("2".into(), Some(other.clone()))].into(),
                },
                11.0,
            )
            .unwrap();

        controller
            .execute(
                DisplayCommand::Clear {
                    token: token.clone(),
                    keys: Some(vec![0]),
                },
                12.0,
            )
            .unwrap();
        assert_eq!(controller.scene(0, 12.0).unwrap().0, Scene::default());
        assert_eq!(
            controller.scene(1, 12.0).unwrap().0,
            second,
            "subset clear must preserve other scenes in the same lease"
        );

        let invalid = controller
            .execute(
                DisplayCommand::Clear {
                    token: token.clone(),
                    keys: Some(vec![1, 2]),
                },
                13.0,
            )
            .unwrap_err();
        assert_eq!(invalid.status, 400);
        assert_eq!(
            controller.scene(1, 13.0).unwrap().0.title,
            "second",
            "an invalid mixed-owner subset must be atomic"
        );
        assert_eq!(
            controller.scene(2, 13.0).unwrap().0,
            other,
            "clear must not change another owner's scene"
        );

        controller
            .execute(
                DisplayCommand::Clear {
                    token: token.clone(),
                    keys: None,
                },
                14.0,
            )
            .unwrap();
        assert_eq!(controller.scene(0, 14.0).unwrap().0, Scene::default());
        assert_eq!(
            controller.scene(1, 14.0).unwrap().0,
            Scene::default(),
            "omitted keys clear every scene in the lease"
        );
        let state = controller.execute(DisplayCommand::State {}, 14.0).unwrap();
        assert_eq!(
            state["leases"].as_array().unwrap().len(),
            2,
            "clear retains both leases"
        );
        assert!(
            state["leases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|lease| lease.get("token").is_none())
        );
    }

    #[test]
    fn display_overlay_restores_native_faces_without_touching_provider_sessions() {
        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        core.settings.motion = crate::model::Motion::Reduced;
        core.apply(crate::model::Incoming {
            agent: "codex".into(),
            event: None,
            body: json!({"session":"tracked-session","state":"thinking","title":"Tracked"}),
        });
        let session_count = core.visible().len();
        assert_eq!(session_count, 1);
        let result = core
            .display_command(DisplayCommand::Claim {
                owner: "test-client".into(),
                keys: vec![0],
                ttl_seconds: Some(1.0),
            })
            .unwrap();
        let token = result["token"].as_str().unwrap().to_owned();
        core.display_command(DisplayCommand::Set {
            token,
            keys: [(
                "0".into(),
                Some(Scene {
                    title: "External".into(),
                    ..Scene::default()
                }),
            )]
            .into(),
        })
        .unwrap();
        assert!(
            matches!(
                core.faces()[0],
                crate::model::KeyFace::Controlled(_, _, true)
            ),
            "reduced-motion preference reaches controlled render faces"
        );

        core.apply(crate::model::Incoming {
            agent: "codex".into(),
            event: None,
            body: json!({"session":"tracked-session","state":"done","title":"Completed underneath","text":"Latest provider text","_unread":1}),
        });
        assert!(
            matches!(
                core.faces()[0],
                crate::model::KeyFace::Controlled(_, _, true)
            ),
            "provider updates remain hidden behind the active display lease"
        );
        assert_eq!(
            core.visible()[0].activity,
            Activity::Done,
            "the provider session state still updates beneath the overlay"
        );

        core.display.prune(crate::model::now() + 2.0);
        match &core.faces()[0] {
            crate::model::KeyFace::Session(face) => {
                assert_eq!(
                    face.activity,
                    Activity::Done,
                    "expiry must reveal the latest provider state, not the old face"
                );
                assert_eq!(face.last_message, "Latest provider text");
                assert_eq!(face.unread, 1);
            }
            _ => panic!("expiring a lease restores the tracked native face"),
        }
        assert_eq!(core.visible().len(), session_count);
        assert!(core.actions.is_empty());
    }

    #[test]
    fn held_press_survives_other_key_updates_but_not_its_own_replacement() {
        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        let claim = core
            .display_command(DisplayCommand::Claim {
                owner: "test-client".into(),
                keys: vec![0, 1],
                ttl_seconds: Some(60.0),
            })
            .unwrap();
        let token = claim["token"].as_str().unwrap().to_owned();

        core.key_down(0);
        core.display_command(DisplayCommand::Set {
            token: token.clone(),
            keys: [(
                "1".into(),
                Some(Scene {
                    title: "other key update".into(),
                    ..Scene::default()
                }),
            )]
            .into(),
        })
        .unwrap();
        core.key_up(0);
        let events = core
            .display_command(DisplayCommand::Events {
                token: token.clone(),
            })
            .unwrap();
        assert_eq!(
            events["events"].as_array().unwrap().len(),
            1,
            "updating another owned key must preserve the captured press generation"
        );

        core.key_down(0);
        core.display_command(DisplayCommand::Set {
            token: token.clone(),
            keys: [(
                "0".into(),
                Some(Scene {
                    title: "same key replaced".into(),
                    ..Scene::default()
                }),
            )]
            .into(),
        })
        .unwrap();
        core.key_up(0);
        let events = core
            .display_command(DisplayCommand::Events { token })
            .unwrap();
        assert!(
            events["events"].as_array().unwrap().is_empty(),
            "replacing the pressed key must reject its stale generation"
        );

        core.display_command(DisplayCommand::Claim {
            owner: "old-owner".into(),
            keys: vec![2],
            ttl_seconds: Some(60.0),
        })
        .unwrap();
        core.key_down(2);
        core.release_display();
        let new_token = core
            .display_command(DisplayCommand::Claim {
                owner: "new-owner".into(),
                keys: vec![2],
                ttl_seconds: Some(60.0),
            })
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        core.key_up(2);
        assert!(
            core.display_command(DisplayCommand::Events { token: new_token })
                .unwrap()["events"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a press captured before release must not reach a replacement lease"
        );
        assert_eq!(core.visible().len(), 0);
        assert!(core.actions.is_empty());
    }

    #[test]
    fn scenes_preserve_native_activity_and_animate_custom_gradients() {
        let native = Scene {
            kind: "native".into(),
            activity: "thinking".into(),
            ..Scene::default()
        };
        assert_eq!(native.native_face().activity, Activity::Thinking);
        assert!(native.animated());

        let custom = Scene {
            animation: "gradient".into(),
            ..Scene::default()
        };
        assert!(custom.animated());
        assert_ne!(custom.sample(0.5, 0.5, 0.0), custom.sample(0.5, 0.5, 1.0));
        let static_scene = Scene::default();
        assert!(!static_scene.animated());
        assert_eq!(
            static_scene.sample(0.5, 0.5, 0.0),
            static_scene.sample(0.5, 0.5, 1.0)
        );
    }
}
