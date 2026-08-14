pub mod app;
pub mod atomic_f32;
pub mod channel_pairing;
pub mod config;
pub mod device_kind;
pub mod device_widget;
pub mod dropdown_widget;
pub mod event;
pub mod help;
pub mod hidden_state;
pub mod input;
pub mod meter;
pub mod node_widget;
pub mod object_list;
pub mod opt;
pub mod view;
pub mod wirehose;

#[cfg(feature = "trace")]
pub mod trace;

#[cfg(test)]
mod mock {
    use crate::wirehose::{CommandSender, ObjectId, PeakProcessor};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::{atomic::AtomicBool, Arc};

    // Variants intentionally share the `Node` prefix, mirroring
    // CommandSender's own node_capture_start/node_capture_stop/
    // node_volumes method names.
    #[allow(clippy::enum_variant_names)]
    #[derive(Debug, PartialEq)]
    pub enum MockCommand {
        NodeCaptureStart(ObjectId),
        NodeCaptureStop(ObjectId),
        NodeVolumes(ObjectId, Vec<f32>),
    }

    #[derive(Default)]
    pub struct WirehoseHandle<'a> {
        commands: Option<&'a RefCell<VecDeque<MockCommand>>>,
    }

    impl<'a> WirehoseHandle<'a> {
        pub fn with_commands(
            commands: &'a RefCell<VecDeque<MockCommand>>,
        ) -> Self {
            Self {
                commands: Some(commands),
            }
        }
    }

    impl CommandSender for WirehoseHandle<'_> {
        fn node_capture_start(
            &self,
            object_id: ObjectId,
            _object_serial: u64,
            _capture_sink: bool,
            _peaks_dirty: Arc<AtomicBool>,
            _peak_processor: Option<Arc<dyn PeakProcessor>>,
        ) {
            if let Some(commands) = self.commands {
                commands
                    .borrow_mut()
                    .push_back(MockCommand::NodeCaptureStart(object_id));
            }
        }
        fn node_capture_stop(&self, object_id: ObjectId) {
            if let Some(commands) = self.commands {
                commands
                    .borrow_mut()
                    .push_back(MockCommand::NodeCaptureStop(object_id));
            }
        }
        fn node_mute(&self, _object_id: ObjectId, _mute: bool) {}
        fn node_volumes(&self, object_id: ObjectId, volumes: Vec<f32>) {
            if let Some(commands) = self.commands {
                commands
                    .borrow_mut()
                    .push_back(MockCommand::NodeVolumes(object_id, volumes));
            }
        }
        fn device_mute(
            &self,
            _object_id: ObjectId,
            _route_index: i32,
            _route_device: i32,
            _mute: bool,
        ) {
        }
        fn device_set_profile(
            &self,
            _object_id: ObjectId,
            _profile_index: i32,
        ) {
        }
        fn device_set_route(
            &self,
            _object_id: ObjectId,
            _route_index: i32,
            _route_device: i32,
        ) {
        }
        fn device_volumes(
            &self,
            _object_id: ObjectId,
            _route_index: i32,
            _route_device: i32,
            _volumes: Vec<f32>,
        ) {
        }
        fn metadata_set_property(
            &self,
            _object_id: ObjectId,
            _subject: u32,
            _key: String,
            _type_: Option<String>,
            _value: Option<String>,
        ) {
        }
    }
}
