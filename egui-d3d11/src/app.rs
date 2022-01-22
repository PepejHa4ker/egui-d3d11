use egui::{epaint::Vertex, ClippedMesh, CtxRef, Modifiers, Pos2, RawInput, Rect};
use parking_lot::Mutex;
use std::{
    intrinsics::transmute,
    mem::{size_of, zeroed},
    ptr::null_mut as null,
};
use windows::{
    core::HRESULT,
    Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        Graphics::{
            Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            Direct3D11::{
                ID3D11Device, ID3D11DeviceContext, ID3D11InputLayout, ID3D11RenderTargetView,
                ID3D11Texture2D, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_BLEND_DESC,
                D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
                D3D11_BLEND_SRC_ALPHA, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_INPUT_ELEMENT_DESC,
                D3D11_INPUT_PER_VERTEX_DATA, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_VIEWPORT,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32_UINT,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                },
                IDXGISwapChain,
            },
        },
        System::WindowsProgramming::NtQuerySystemTime,
        UI::WindowsAndMessaging::GetClientRect,
    },
};

use crate::{mesh::MeshBuffers, shader::CompiledShaders};

type FnResizeBuffers =
    unsafe extern "stdcall" fn(IDXGISwapChain, u32, u32, u32, DXGI_FORMAT, u32) -> HRESULT;

#[allow(unused)]
pub struct DirectX11App {
    render_view: Mutex<ID3D11RenderTargetView>,
    input_layout: ID3D11InputLayout,
    shaders: CompiledShaders,
    ui: fn(&CtxRef),
    ctx: Mutex<CtxRef>,
    hwnd: HWND,
}

impl DirectX11App {
    #[inline]
    fn get_screen_size(&self) -> Pos2 {
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(self.hwnd, &mut rect);
        }
        Pos2 {
            x: (rect.right - rect.left) as f32,
            y: (rect.bottom - rect.top) as f32,
        }
    }

    #[inline]
    fn get_screen_rect(&self) -> Rect {
        Rect {
            min: Pos2::ZERO,
            max: self.get_screen_size(),
        }
    }

    #[inline]
    fn get_system_time() -> f64 {
        let mut time = 0;
        unsafe {
            if NtQuerySystemTime(&mut time).is_err() {
                if !cfg!(feature = "no-msgs") {
                    panic!("Failed to get system's time.");
                } else {
                    unreachable!()
                }
            }
        }
        time as f64
    }

    const LAYOUT_ELEMENTS: [D3D11_INPUT_ELEMENT_DESC; 3] = [
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: c_str!("POSITION"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: 0,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: c_str!("TEXCOORD"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: c_str!("COLOR"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            InputSlot: 0,
            AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
    ];

    fn create_input_layout(shaders: &CompiledShaders, device: &ID3D11Device) -> ID3D11InputLayout {
        unsafe {
            expect!(
                device.CreateInputLayout(
                    Self::LAYOUT_ELEMENTS.as_ptr(),
                    Self::LAYOUT_ELEMENTS.len() as _,
                    shaders.blobs.vertex.GetBufferPointer(),
                    shaders.blobs.vertex.GetBufferSize()
                ),
                "Failed to create input layout."
            )
        }
    }

    fn normalize_meshes(&self, meshes: &mut Vec<ClippedMesh>) {
        let mut screen_half = self.get_screen_size();
        screen_half.x /= 2.;
        screen_half.y /= 2.;

        meshes
            .iter_mut()
            .map(|m| &mut m.1.vertices)
            .flatten()
            .for_each(|v| {
                v.pos.x -= screen_half.x;
                v.pos.y -= screen_half.y;

                v.pos.x /= screen_half.x;
                v.pos.y /= -screen_half.y;
            })
    }

    fn set_blend_state(&self, device: &ID3D11Device, context: &ID3D11DeviceContext) {
        unsafe {
            let mut targets: [D3D11_RENDER_TARGET_BLEND_DESC; 8] = zeroed();
            targets[0].BlendEnable = true.into();
            targets[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
            targets[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
            targets[0].BlendOp = D3D11_BLEND_OP_ADD;
            targets[0].SrcBlendAlpha = D3D11_BLEND_ONE;
            targets[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
            targets[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
            targets[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL as _;

            let blend_desc = D3D11_BLEND_DESC {
                AlphaToCoverageEnable: false.into(),
                IndependentBlendEnable: false.into(),
                RenderTarget: targets,
            };

            let state = expect!(
                device.CreateBlendState(&blend_desc),
                "Failed to create blend state."
            );
            context.OMSetBlendState(&state, [0., 0., 0., 0.].as_ptr(), 0xffffffff);
        }
    }

    fn set_viewports(&self, context: &ID3D11DeviceContext) {
        let size = self.get_screen_size();
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.,
            TopLeftY: 0.,
            Width: size.x,
            Height: size.y,
            MinDepth: 0.,
            MaxDepth: 1.,
        };

        unsafe {
            context.RSSetViewports(1, &viewport);
        }
    }

    fn render_meshes(
        &self,
        mut meshes: Vec<ClippedMesh>,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) {
        self.normalize_meshes(&mut meshes);
        self.set_viewports(context);
        self.set_blend_state(device, context);

        let view_lock = &mut *self.render_view.lock();

        unsafe {
            context.OMSetRenderTargets(1, transmute(view_lock), None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.IASetInputLayout(&self.input_layout);

            for mesh in &meshes {
                let buffers = MeshBuffers::new(device, &mesh);

                context.IASetVertexBuffers(
                    0,
                    1,
                    &Some(buffers.vertex),
                    &(size_of::<Vertex>() as _),
                    &0,
                );
                context.IASetIndexBuffer(&buffers.index, DXGI_FORMAT_R32_UINT, 0);

                context.VSSetShader(&self.shaders.vertex, null(), 0);
                context.PSSetShader(&self.shaders.pixel, null(), 0);

                context.DrawIndexed(mesh.1.indices.len() as _, 0, 0);
            }
        }
    }
}

impl DirectX11App {
    pub fn new(ui: fn(&CtxRef), swap_chain: &IDXGISwapChain, device: &ID3D11Device) -> Self {
        unsafe {
            let hwnd = expect!(
                swap_chain.GetDesc(),
                "Failed to get swapchain's descriptor."
            )
            .OutputWindow;

            if hwnd.is_invalid() {
                if !cfg!(feature = "no-msgs") {
                    panic!("Invalid output window descriptor.");
                } else {
                    unreachable!()
                }
            }

            let back_buffer: ID3D11Texture2D = expect!(
                swap_chain.GetBuffer(0),
                "Failed to get swapchain's back buffer"
            );

            let render_view = expect!(
                device.CreateRenderTargetView(&back_buffer, null()),
                "Failed to create render target view."
            );

            let shaders = CompiledShaders::new(device);
            let input_layout = Self::create_input_layout(&shaders, device);

            Self {
                render_view: Mutex::new(render_view),
                ctx: Mutex::new(CtxRef::default()),
                input_layout,
                shaders,
                hwnd,
                ui,
            }
        }
    }

    pub fn present(&self, swap_chain: &IDXGISwapChain, _sync_flags: u32, _interval: u32) {
        let (device, context) = get_device_context(swap_chain);

        let ctx_lock = &mut *self.ctx.lock();

        let input = RawInput {
            screen_rect: Some(self.get_screen_rect()),
            pixels_per_point: Some(1.),
            time: Some(Self::get_system_time()),
            predicted_dt: 1. / 60.,
            modifiers: Modifiers::default(),
            events: vec![],
            hovered_files: vec![],
            dropped_files: vec![],
        };

        let (_output, shapes) = ctx_lock.run(input, self.ui);
        let meshes = ctx_lock.tessellate(shapes);

        self.render_meshes(meshes, &device, &context);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resize_buffers(
        &self,
        swap_chain: &IDXGISwapChain,
        buffer_count: u32,
        width: u32,
        height: u32,
        new_format: DXGI_FORMAT,
        swap_chain_flags: u32,
        original: FnResizeBuffers,
    ) -> HRESULT {
        unsafe {
            let view_lock = &mut *self.render_view.lock();
            std::ptr::drop_in_place(view_lock);

            let result = original(
                swap_chain.clone(),
                buffer_count,
                width,
                height,
                new_format,
                swap_chain_flags,
            );

            let backbuffer: ID3D11Texture2D = expect!(
                swap_chain.GetBuffer(0),
                "Failed to get swapchain's backbuffer."
            );

            let device: ID3D11Device =
                expect!(swap_chain.GetDevice(), "Failed to get swapchain's device.");

            let new_view = expect!(
                device.CreateRenderTargetView(&backbuffer, null()),
                "Failed to create render target view."
            );

            *view_lock = new_view;
            result
        }
    }

    pub fn wnd_proc(&self, _hwnd: HWND, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> bool {
        true
    }
}

#[inline]
fn get_device_context(swapchain: &IDXGISwapChain) -> (ID3D11Device, ID3D11DeviceContext) {
    unsafe {
        let device: ID3D11Device =
            expect!(swapchain.GetDevice(), "Failed to get swapchain's device");

        let mut context = None;
        device.GetImmediateContext(&mut context);

        (
            device,
            expect!(context, "Failed to get device's immediate context."),
        )
    }
}
