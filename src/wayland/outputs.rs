use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output::{self, Transform, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Dispatch, QueueHandle, WEnum};

/// wl_output version 4 is the first to announce the output's name.
const OUTPUT_VERSION: u32 = 4;
const OUTPUT_INTERFACE: &str = "wl_output";

/// One output as far as its events have described it.
#[derive(Debug, Clone)]
pub(crate) struct TrackedOutput {
    pub(crate) global: u32,
    pub(crate) output: WlOutput,
    pub(crate) name: Option<String>,
    pub(crate) mode: (i32, i32),
    pub(crate) scale: i32,
    pub(crate) transform: Transform,
}

impl TrackedOutput {
    /// Size in the logical coordinate space pointer and touch positions use.
    pub(crate) fn logical_size(&self) -> (f64, f64) {
        logical_size(self.mode, self.scale, self.transform)
    }
}

/// Every wl_output, bound as it appears, so outputs can be found by the name a virtual output
/// or the user gave them.
#[derive(Debug, Default)]
pub(crate) struct Outputs {
    outputs: Vec<TrackedOutput>,
}

impl Outputs {
    pub(crate) fn bind_existing<S>(globals: &GlobalList, queue: &QueueHandle<S>) -> Self
    where
        S: Dispatch<WlOutput, u32> + 'static,
    {
        let mut outputs = Self::default();
        globals.contents().with_list(|list| {
            for global in list
                .iter()
                .filter(|global| global.interface == OUTPUT_INTERFACE)
            {
                outputs.bind(globals.registry(), global.name, global.version, queue);
            }
        });
        outputs
    }

    fn bind<S>(&mut self, registry: &WlRegistry, global: u32, version: u32, queue: &QueueHandle<S>)
    where
        S: Dispatch<WlOutput, u32> + 'static,
    {
        let output = registry.bind(global, version.min(OUTPUT_VERSION), queue, global);
        self.outputs.push(TrackedOutput {
            global,
            output,
            name: None,
            mode: (0, 0),
            scale: 1,
            transform: Transform::Normal,
        });
    }

    /// The named output, or the first one when no name is given.
    pub(crate) fn find(&self, name: Option<&str>) -> Option<&TrackedOutput> {
        match name {
            Some(name) => self
                .outputs
                .iter()
                .find(|output| output.name.as_deref() == Some(name)),
            None => self.outputs.first(),
        }
    }

    pub(crate) fn find_output(&self, output: &WlOutput) -> Option<&TrackedOutput> {
        self.outputs
            .iter()
            .find(|tracked| tracked.output == *output)
    }

    pub(crate) fn on_registry_event<S>(
        &mut self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        queue: &QueueHandle<S>,
    ) where
        S: Dispatch<WlOutput, u32> + 'static,
    {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == OUTPUT_INTERFACE => self.bind(registry, name, version, queue),
            wl_registry::Event::GlobalRemove { name } => {
                self.outputs.retain(|output| output.global != name);
            }
            _ => {}
        }
    }

    pub(crate) fn on_output_event(&mut self, global: u32, event: wl_output::Event) {
        let Some(output) = self
            .outputs
            .iter_mut()
            .find(|output| output.global == global)
        else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.name = Some(name),
            wl_output::Event::Scale { factor } => output.scale = factor.max(1),
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => output.mode = (width, height),
            wl_output::Event::Geometry {
                transform: WEnum::Value(transform),
                ..
            } => output.transform = transform,
            _ => {}
        }
    }
}

/// Implements the registry and wl_output handlers for a worker state holding [`Outputs`].
macro_rules! track_outputs {
    ($state:ty, $field:ident) => {
        impl
            wayland_client::Dispatch<
                wayland_client::protocol::wl_registry::WlRegistry,
                wayland_client::globals::GlobalListContents,
            > for $state
        {
            fn event(
                state: &mut Self,
                registry: &wayland_client::protocol::wl_registry::WlRegistry,
                event: wayland_client::protocol::wl_registry::Event,
                _: &wayland_client::globals::GlobalListContents,
                _: &wayland_client::Connection,
                queue: &wayland_client::QueueHandle<Self>,
            ) {
                state.$field.on_registry_event(registry, event, queue);
            }
        }

        impl wayland_client::Dispatch<wayland_client::protocol::wl_output::WlOutput, u32>
            for $state
        {
            fn event(
                state: &mut Self,
                _: &wayland_client::protocol::wl_output::WlOutput,
                event: wayland_client::protocol::wl_output::Event,
                global: &u32,
                _: &wayland_client::Connection,
                _: &wayland_client::QueueHandle<Self>,
            ) {
                state.$field.on_output_event(*global, event);
            }
        }
    };
}
pub(crate) use track_outputs;

fn logical_size(mode: (i32, i32), scale: i32, transform: Transform) -> (f64, f64) {
    let scale = f64::from(scale.max(1));
    let (width, height) = (f64::from(mode.0) / scale, f64::from(mode.1) / scale);
    match transform {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => {
            (height, width)
        }
        _ => (width, height),
    }
}

/// Maps a point given as fractions of an output (0.0 to 1.0 each way) onto its logical space.
pub(crate) fn logical_point(fraction: (f64, f64), logical_size: (f64, f64)) -> (f64, f64) {
    (
        fraction.0.clamp(0.0, 1.0) * logical_size.0,
        fraction.1.clamp(0.0, 1.0) * logical_size.1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_size_divides_by_scale_and_swaps_when_rotated() {
        assert_eq!(
            logical_size((3840, 2160), 2, Transform::Normal),
            (1920.0, 1080.0)
        );
        assert_eq!(
            logical_size((2560, 1600), 1, Transform::_90),
            (1600.0, 2560.0)
        );
        assert_eq!(
            logical_size((100, 50), 0, Transform::Flipped),
            (100.0, 50.0)
        );
    }

    #[test]
    fn fractions_map_onto_the_output_and_are_clamped() {
        assert_eq!(logical_point((0.5, 0.25), (1920.0, 1080.0)), (960.0, 270.0));
        assert_eq!(logical_point((-1.0, 2.0), (1920.0, 1080.0)), (0.0, 1080.0));
    }
}
