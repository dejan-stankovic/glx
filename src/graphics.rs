use log::*;
extern crate env_logger;
extern crate wgpu;

use euclid::{Box2D, Point2D};
use lyon::tessellation::*;

use crate::graphics;
use shaderc;

use euclid::*;

use lyon::path::Path;
use palette::{Gradient, Lch, Srgb};

/// Return a color scale from cold at 0 to warm at 1. This will draw attention towards higher
/// values.
///
/// This scale has high distinguishability.
pub fn scale_temperature(mut scalar: f32, n_chunks: f32) -> [f32; 3] {
    scalar = (scalar * n_chunks).floor() / (n_chunks - 1.0);
    let lightness = 70.0;
    let chroma = 90.0;
    match Srgb::from(
        Gradient::new(vec![
            Lch::new(lightness, chroma, 60.0),
            Lch::new(lightness, chroma, 280.0),
        ])
        .get(scalar),
    )
    .into_components()
    {
        (r, g, b) => [r, g, b],
    }
}

/// Return a color scale from drab at 0 to colorful at 1. This will strongly draw attention towards
/// higher values.
///
/// This color scale has medium distinguishability.
pub fn scale_chroma(mut scalar: f32, n_chunks: f32) -> [f32; 3] {
    // Quantize the colors
    scalar = (scalar * n_chunks).floor() / (n_chunks - 1.0);
    let lightness = 70.0;
    let chroma = 90.0;
    match Srgb::from(
        Gradient::new(vec![
            Lch::new(lightness, 0.0, 60.0),
            Lch::new(lightness, 90.0, 60.0),
        ])
            .get(scalar),
    )
        .into_components()
        {
            (r, g, b) => [r, g, b],
        }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Vertex {
    _pos: [f32; 2],
    _color: [f32; 3],
}

enum MyPath {
    Filled(Path),
    Stroked { path: Path, width: f32 },
}

pub struct StyledGeom {
    pub geom: Geom,
    pub color: [f32; 3],
}

#[derive(Clone, Debug)]
pub enum Geom {
    Point(Point2D<f32>),
    Lines {
        points: Vec<Point2D<f32>>,
        width: f32,
    },
    Polygon(Vec<Point2D<f32>>), // don't repeat the first point
}

pub enum PointStyle {
    Circle { radius: f32 },
}

fn transform_viewport(
    point: &Point2D<f32>,
    viewport: &Box2D<f32>,
    aspect_ratio: f32,
) -> Point2D<f32> {
    Point2D::new(
        (2.0 * (point.x - viewport.min.x) / (viewport.max.x - viewport.min.x) - 1.0) / aspect_ratio,
        2.0 * (point.y - viewport.min.y) / (viewport.max.y - viewport.min.y) - 1.0,
    )
}

fn transform_viewport_1d(len: f32, viewport: &Box2D<f32>) -> f32 {
    2.0 * len / (viewport.max.y - viewport.min.y)
}

// Stages:
// 1. True space coordinates, i.e. locations on the Somerville map
//     viewport transform
// 2. Drawing space coordinates
//     screen transform
// 3. Screen coordinates, i.e. pixels
//    hardcoded basically divide by screen resoltion
// 4. wgpu -1 to 1 coordinates?
fn geom_to_path(geom: Geom, viewport: Box2D<f32>, screen: Vector2D<usize>) -> MyPath {
    let mut builder = Path::builder();

    //    let original_to_drawing = |x| x * screen;
    let aspect_ratio = screen.x as f32 / screen.y as f32;

    match geom {
        Geom::Point(point) => {
            // 3px diameter is good
            let radius_px = 10.0;
            let point = transform_viewport(&point, &viewport, aspect_ratio);
            builder.move_to(point + Vector2D::new(radius_px / screen.x as f32, 0.0));
            builder.arc(
                point,
                Vector2D::new(radius_px / screen.x as f32, radius_px / screen.y as f32),
                Angle::two_pi(),
                Angle::zero(),
            );
            builder.close();
            MyPath::Filled(builder.build())
        }
        Geom::Lines { points, width } => {
            debug_assert!(points.len() >= 2);
            builder.move_to(transform_viewport(&points[0], &viewport, aspect_ratio));
            for point in &points[1..] {
                builder.line_to(transform_viewport(&point, &viewport, aspect_ratio));
            }
            MyPath::Stroked {
                path: builder.build(),
                width: transform_viewport_1d(width, &viewport),
            }
        }
        Geom::Polygon(points) => {
            debug_assert!(points.len() >= 3);
            builder.move_to(transform_viewport(&points[0], &viewport, aspect_ratio));
            for point in &points[1..] {
                builder.line_to(transform_viewport(&point, &viewport, aspect_ratio));
            }
            builder.close();
            MyPath::Filled(builder.build())
        }
    }
}

fn create_vertices(
    styled_geoms: Vec<StyledGeom>,
    screen: Vector2D<usize>,
    viewport: Box2D<f32>,
) -> (Vec<Vertex>, Vec<u32>) {
    // Will contain the result of the tessellation.
    let mut geometry: VertexBuffers<Vertex, u32> = VertexBuffers::new();

    let mut fill_tessellator = FillTessellator::new();
    let mut stroke_tessellator = StrokeTessellator::new();

    let tolerance = 0.0001;
    let fill_options = FillOptions::DEFAULT
        .with_normals(false)
        .with_tolerance(tolerance);
    let stroke_options = StrokeOptions::DEFAULT
        .with_line_width(0.002)
        .with_tolerance(tolerance);

    for styled_geom in styled_geoms.iter() {
        match geom_to_path(styled_geom.geom.clone(), viewport, screen) {
            MyPath::Filled(path) => {
                fill_tessellator
                    .tessellate_path(
                        path.into_iter(),
                        &fill_options,
                        &mut BuffersBuilder::new(&mut geometry, |vertex: FillVertex| Vertex {
                            _pos: [vertex.position.x, vertex.position.y],
                            _color: styled_geom.color,
                        }),
                    )
                    .unwrap();
            }
            MyPath::Stroked { path, width } => {
                stroke_tessellator
                    .tessellate_path(
                        path.into_iter(),
                        &stroke_options.with_line_width(width),
                        &mut BuffersBuilder::new(&mut geometry, |vertex: StrokeVertex| Vertex {
                            _pos: [vertex.position.x, vertex.position.y],
                            _color: styled_geom.color,
                        }),
                    )
                    .unwrap();
            }
        }
    }

    info!(
        "{} vertices, {} indices",
        geometry.vertices.len(),
        geometry.indices.len()
    );

    (geometry.vertices, geometry.indices)
}

use log::info;

#[allow(dead_code)]
pub fn cast_slice<T>(data: &[T]) -> &[u8] {
    use std::mem::size_of;
    use std::slice::from_raw_parts;

    unsafe { from_raw_parts(data.as_ptr() as *const u8, data.len() * size_of::<T>()) }
}

pub fn glsl_to_spirv(name: &str, source: &str, kind: shaderc::ShaderKind) -> Vec<u8> {
    let mut compiler = shaderc::Compiler::new().unwrap();
    Vec::from(
        compiler
            .compile_into_spirv(source, kind, name, "main", None)
            .unwrap()
            .as_binary_u8(),
    )
}

pub trait Example {
    fn init(sc_desc: &wgpu::SwapChainDescriptor, device: &mut wgpu::Device) -> Self;
    fn resize(&mut self, sc_desc: &wgpu::SwapChainDescriptor, device: &mut wgpu::Device);
    fn update(&mut self, event: wgpu::winit::WindowEvent);
    fn render(&mut self, frame: &wgpu::SwapChainOutput, device: &mut wgpu::Device);
}

pub fn leggo(styled_geoms: Vec<StyledGeom>, viewport: Box2D<f32>) {
    debug!("Initializing WGPU...");
    let instance = wgpu::Instance::new();

    let adapter = instance.get_adapter(&wgpu::AdapterDescriptor {
        power_preference: wgpu::PowerPreference::LowPower,
    });

    let mut device = adapter.request_device(&wgpu::DeviceDescriptor {
        extensions: wgpu::Extensions {
            anisotropic_filtering: false,
        },
        limits: wgpu::Limits::default(),
    });

    debug!("building shaders...");
    let vs_bytes = graphics::glsl_to_spirv("graphics.vert", include_str!("shader/graphics.vert"), shaderc::ShaderKind::Vertex);
    let fs_bytes = graphics::glsl_to_spirv("graphics.frag", include_str!("shader/graphics.frag"), shaderc::ShaderKind::Fragment);
    let vs_module = device.create_shader_module(&vs_bytes);
    let fs_module = device.create_shader_module(&fs_bytes);

    let vertex_size = std::mem::size_of::<Vertex>();

    // Ways to get dimensions:
    // - from actual window size when previewing
    // - from an intended px dimension
    // - from intended real world dimension + DPI
    let screen = Vector2D::new(2880, 1800);

    let (vertex_data, index_data) = create_vertices(styled_geoms, screen, viewport);
    assert!(!vertex_data.is_empty());

    let vertex_buf = device
        .create_buffer_mapped(vertex_data.len(), wgpu::BufferUsage::VERTEX)
        .fill_from_slice(&vertex_data);

    let index_buf = device
        .create_buffer_mapped(index_data.len(), wgpu::BufferUsage::INDEX)
        .fill_from_slice(&index_data);

    let bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { bindings: &[] });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        layout: &bind_group_layout,
        bindings: &[],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        bind_group_layouts: &[&bind_group_layout],
    });

    let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        layout: &pipeline_layout,
        vertex_stage: wgpu::PipelineStageDescriptor {
            module: &vs_module,
            entry_point: "main",
        },
        fragment_stage: Some(wgpu::PipelineStageDescriptor {
            module: &fs_module,
            entry_point: "main",
        }),
        rasterization_state: wgpu::RasterizationStateDescriptor {
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: wgpu::CullMode::None,
            depth_bias: 0,
            depth_bias_slope_scale: 0.0,
            depth_bias_clamp: 0.0,
        },
        primitive_topology: wgpu::PrimitiveTopology::TriangleList,
        color_states: &[wgpu::ColorStateDescriptor {
            format: wgpu::TextureFormat::Bgra8Unorm,
            color_blend: wgpu::BlendDescriptor::REPLACE,
            alpha_blend: wgpu::BlendDescriptor::REPLACE,
            write_mask: wgpu::ColorWrite::ALL,
        }],
        depth_stencil_state: None,
        index_format: wgpu::IndexFormat::Uint32,
        vertex_buffers: &[wgpu::VertexBufferDescriptor {
            stride: vertex_size as u64,
            step_mode: wgpu::InputStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttributeDescriptor {
                    format: wgpu::VertexFormat::Float2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttributeDescriptor {
                    format: wgpu::VertexFormat::Float3,
                    offset: 8, // Because this is preceded by two 4-byte floats?
                    shader_location: 1
                },
            ],
        }],
        sample_count: 1,
    });

    use wgpu::winit::{
        ControlFlow, ElementState, Event, EventsLoop, KeyboardInput, VirtualKeyCode, Window,
        WindowEvent,
    };

    let mut events_loop = EventsLoop::new();
    let window = Window::new(&events_loop).unwrap();
    window.set_fullscreen(Some(window.get_current_monitor()));
    let size = window
        .get_inner_size()
        .unwrap()
        .to_physical(window.get_hidpi_factor());

    let surface = instance.create_surface(&window);
    let mut swap_chain = device.create_swap_chain(
        &surface,
        &wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width: (size.width.round() as u32) * 4,
            height: (size.height.round() as u32) * 4,
        },
    );

    events_loop.run_forever(|event| {
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::KeyboardInput {
                    input:
                        KeyboardInput {
                            virtual_keycode: Some(code),
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => match code {
                    VirtualKeyCode::Escape => return ControlFlow::Break,
                    _ => {}
                },
                WindowEvent::CloseRequested => return ControlFlow::Break,
                _ => {}
            },
            _ => {}
        }

        let frame = swap_chain.get_next_texture();
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { todo: 0 });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[wgpu::RenderPassColorAttachmentDescriptor {
                    attachment: &frame.view,
                    resolve_target: None,
                    load_op: wgpu::LoadOp::Clear,
                    store_op: wgpu::StoreOp::Store,
                    clear_color: wgpu::Color::WHITE,
                }],
                depth_stencil_attachment: None,
            });
            rpass.set_pipeline(&render_pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.set_index_buffer(&index_buf, 0);
            rpass.set_vertex_buffers(&[(&vertex_buf, 0)]);
            rpass.draw_indexed(0..(index_data.len() as u32), 0, 0..1);
        }

        device.get_queue().submit(&[encoder.finish()]);

        ControlFlow::Continue
    });
}
