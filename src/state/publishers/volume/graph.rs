use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::mpsc::SyncSender;

use llts_signaling::message::SetVolume;
use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::node::{Node, NodeListener};
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa::param::ParamType;
use pw::spa::pod::Pod;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use super::props::{default_sink_name, props_pod, SinkProps};
use super::VolumeError;
use crate::state::{LocalState, StateSink};

const DEFAULT_SINK_KEY: &str = "default.audio.sink";
const STEREO: usize = 2;

pub(super) enum GraphCommand {
    Set(SetVolume),
    Quit,
}

struct SinkNode {
    name: String,
    node: Node,
    _listener: NodeListener,
    props: Option<SinkProps>,
}

/// The audio sinks in the PipeWire graph and which one is the default, on the PipeWire thread.
struct Graph {
    sink: StateSink,
    default_sink: Option<String>,
    sinks: HashMap<u32, SinkNode>,
    metadata: Option<(Metadata, MetadataListener)>,
}

impl Graph {
    fn default_node(&self) -> Option<&SinkNode> {
        let name = self.default_sink.as_deref()?;
        self.sinks.values().find(|sink| sink.name == name)
    }

    fn publish(&self) {
        if let Some(props) = self.default_node().and_then(|sink| sink.props.as_ref()) {
            self.sink
                .publish_from_thread(LocalState::Volume(props.volume()));
        }
    }

    fn set(&self, command: SetVolume) {
        let Some(sink) = self.default_node() else {
            return;
        };
        let channels = sink
            .props
            .as_ref()
            .map_or(STEREO, |props| props.channel_volumes.len());
        let Some(bytes) = props_pod(channels, command.percent, command.muted) else {
            return;
        };
        if let Some(pod) = Pod::from_bytes(&bytes) {
            sink.node.set_param(ParamType::Props, 0, pod);
        }
    }
}

/// Runs the graph watcher until [`GraphCommand::Quit`], reporting startup through `ready`.
pub(super) fn run(
    sink: StateSink,
    commands: pw::channel::Receiver<GraphCommand>,
    ready: &SyncSender<Result<(), VolumeError>>,
) -> Result<(), VolumeError> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let graph = Rc::new(RefCell::new(Graph {
        sink,
        default_sink: None,
        sinks: HashMap::new(),
        metadata: None,
    }));

    let on_global = {
        let graph = Rc::downgrade(&graph);
        let registry = registry.downgrade();
        move |global: &GlobalObject<&DictRef>| {
            if let (Some(graph), Some(registry)) = (graph.upgrade(), registry.upgrade()) {
                track_global(&graph, &registry, global);
            }
        }
    };
    let on_remove = {
        let graph = Rc::downgrade(&graph);
        move |id| {
            if let Some(graph) = graph.upgrade() {
                graph.borrow_mut().sinks.remove(&id);
            }
        }
    };
    let _registry_listener = registry
        .add_listener_local()
        .global(on_global)
        .global_remove(on_remove)
        .register();

    let quitting = main_loop.clone();
    let commanded = Rc::clone(&graph);
    let _commands = commands.attach(main_loop.loop_(), move |command| match command {
        GraphCommand::Set(volume) => commanded.borrow().set(volume),
        GraphCommand::Quit => quitting.quit(),
    });
    let _ = ready.send(Ok(()));
    main_loop.run();
    Ok(())
}

fn track_global(
    graph: &Rc<RefCell<Graph>>,
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
) {
    let property = |key: &str| global.props.and_then(|props| props.get(key));
    match global.type_ {
        ObjectType::Node if property("media.class") == Some("Audio/Sink") => {
            if let Some(name) = property("node.name") {
                track_sink(graph, registry, global, name.to_owned());
            }
        }
        ObjectType::Metadata if property("metadata.name") == Some("default") => {
            track_default_metadata(graph, registry, global);
        }
        _ => {}
    }
}

fn track_sink(
    graph: &Rc<RefCell<Graph>>,
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
    name: String,
) {
    let node: Node = match registry.bind(global) {
        Ok(node) => node,
        Err(error) => {
            tracing::debug!(%error, name, "cannot bind an audio sink");
            return;
        }
    };
    let id = global.id;
    let weak = Rc::downgrade(graph);
    let listener = node
        .add_listener_local()
        .param(move |_, param_type, _, _, pod| {
            if param_type == ParamType::Props {
                store_props(
                    &weak,
                    id,
                    pod.and_then(|pod| SinkProps::parse(pod.as_bytes())),
                );
            }
        })
        .register();
    node.subscribe_params(&[ParamType::Props]);
    graph.borrow_mut().sinks.insert(
        id,
        SinkNode {
            name,
            node,
            _listener: listener,
            props: None,
        },
    );
}

fn store_props(graph: &Weak<RefCell<Graph>>, id: u32, props: Option<SinkProps>) {
    let (Some(graph), Some(props)) = (graph.upgrade(), props) else {
        return;
    };
    let mut graph = graph.borrow_mut();
    if let Some(sink) = graph.sinks.get_mut(&id) {
        sink.props = Some(props);
    }
    graph.publish();
}

fn track_default_metadata(
    graph: &Rc<RefCell<Graph>>,
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
) {
    let metadata: Metadata = match registry.bind(global) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::debug!(%error, "cannot bind the default metadata");
            return;
        }
    };
    let weak = Rc::downgrade(graph);
    let listener = metadata
        .add_listener_local()
        .property(move |_, key, _, value| {
            if key == Some(DEFAULT_SINK_KEY)
                && let Some(graph) = weak.upgrade()
            {
                let mut graph = graph.borrow_mut();
                graph.default_sink = value.and_then(default_sink_name).map(str::to_owned);
                graph.publish();
            }
            0
        })
        .register();
    graph.borrow_mut().metadata = Some((metadata, listener));
}
