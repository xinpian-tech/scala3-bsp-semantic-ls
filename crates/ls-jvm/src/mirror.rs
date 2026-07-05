//! The Rust-side mirror of what a PC instance holds — registered targets and
//! open dirty buffers — so a fresh dispatch generation after a wedge can be
//! reconstructed without the editor reopening anything. This mirrors what
//! `ForkedPcBackend` keeps today; it never crosses the boundary except as the
//! replay stream into a new generation.

use ls_pc_abi::payloads::TargetConfig;

/// One open buffer in the mirror: the owning target, its uri, and current text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenBuffer {
    pub target_id: String,
    pub uri: String,
    pub text: String,
}

/// The state to replay into a freshly spawned dispatch generation: every
/// registered target (in registration order) then every open buffer (in open
/// order), each with its latest text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayPlan {
    pub targets: Vec<(String, TargetConfig)>,
    pub buffers: Vec<OpenBuffer>,
}

/// A registered-target + open-buffer mirror. Insertion order is preserved so
/// replay re-registers targets before re-opening the buffers that depend on
/// them.
#[derive(Debug, Default)]
pub struct TargetMirror {
    targets: Vec<(String, TargetConfig)>,
    buffers: Vec<OpenBuffer>,
}

impl TargetMirror {
    pub fn new() -> TargetMirror {
        TargetMirror::default()
    }

    /// Record (or update) a registered target, preserving first-seen order.
    pub fn register_target(&mut self, id: &str, config: TargetConfig) {
        if let Some(slot) = self.targets.iter_mut().find(|(existing, _)| existing == id) {
            slot.1 = config;
        } else {
            self.targets.push((id.to_string(), config));
        }
    }

    /// Record a newly opened buffer (or refresh an already-open uri's text).
    pub fn did_open(&mut self, target_id: &str, uri: &str, text: &str) {
        match self.buffers.iter_mut().find(|b| b.uri == uri) {
            Some(buf) => {
                buf.target_id = target_id.to_string();
                buf.text = text.to_string();
            }
            None => self.buffers.push(OpenBuffer {
                target_id: target_id.to_string(),
                uri: uri.to_string(),
                text: text.to_string(),
            }),
        }
    }

    /// Update the text of an open buffer. A change to an unknown uri is ignored
    /// (nothing to replay for a buffer that was never opened).
    pub fn did_change(&mut self, uri: &str, text: &str) {
        if let Some(buf) = self.buffers.iter_mut().find(|b| b.uri == uri) {
            buf.text = text.to_string();
        }
    }

    /// Drop a closed buffer from the mirror.
    pub fn did_close(&mut self, uri: &str) {
        self.buffers.retain(|b| b.uri != uri);
    }

    /// The current replay stream for a new dispatch generation.
    pub fn replay_plan(&self) -> ReplayPlan {
        ReplayPlan {
            targets: self.targets.clone(),
            buffers: self.buffers.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(bsp_id: &str) -> TargetConfig {
        TargetConfig {
            bsp_id: bsp_id.to_string(),
            scala_version: "3.8.4".to_string(),
            classpath: vec![],
            scalac_options: vec![],
            source_dirs: vec![],
        }
    }

    #[test]
    fn replay_plan_carries_targets_then_open_buffers_in_order() {
        let mut mirror = TargetMirror::new();
        mirror.register_target("a", config("a"));
        mirror.register_target("b", config("b"));
        mirror.did_open("a", "file:///a.scala", "package a");
        mirror.did_open("b", "file:///b.scala", "package b");
        mirror.did_change("file:///a.scala", "package a\nval x = 1");

        let plan = mirror.replay_plan();
        assert_eq!(
            plan.targets
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(plan.buffers.len(), 2);
        assert_eq!(plan.buffers[0].uri, "file:///a.scala");
        assert_eq!(plan.buffers[0].text, "package a\nval x = 1"); // latest text
        assert_eq!(plan.buffers[1].uri, "file:///b.scala");
    }

    #[test]
    fn re_registering_a_target_updates_config_without_reordering() {
        let mut mirror = TargetMirror::new();
        mirror.register_target("a", config("a"));
        mirror.register_target("b", config("b"));
        mirror.register_target("a", config("a-v2"));
        let plan = mirror.replay_plan();
        assert_eq!(plan.targets[0].0, "a");
        assert_eq!(plan.targets[0].1.bsp_id, "a-v2");
        assert_eq!(plan.targets[1].0, "b");
    }

    #[test]
    fn closed_buffers_are_not_replayed() {
        let mut mirror = TargetMirror::new();
        mirror.register_target("a", config("a"));
        mirror.did_open("a", "file:///a.scala", "x");
        mirror.did_open("a", "file:///b.scala", "y");
        mirror.did_close("file:///a.scala");
        let plan = mirror.replay_plan();
        assert_eq!(plan.buffers.len(), 1);
        assert_eq!(plan.buffers[0].uri, "file:///b.scala");
    }

    #[test]
    fn did_change_on_an_unopened_uri_is_ignored() {
        let mut mirror = TargetMirror::new();
        mirror.did_change("file:///ghost.scala", "text");
        assert!(mirror.replay_plan().buffers.is_empty());
    }
}
