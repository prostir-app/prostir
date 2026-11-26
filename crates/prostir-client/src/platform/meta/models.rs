//! The runtime's models of what the person holds (step 8.6): a controller as a glTF
//! binary (`XR_FB_render_model`), a hand as a mesh with its bones
//! (`XR_FB_hand_tracking_mesh`). Asked once, when the controllers or the hands come
//! (never in a frame of its own: each call takes a few milliseconds and memory); the
//! core reads and checks what came (`model::glb`, `model::hand`).

use std::ffi::CStr;
use std::ptr;

use log::{info, warn};
use openxr as xr;

use crate::model::hand::{Given, HandMesh, JOINTS};
use crate::platform::openxr::{Gfx, quat, vec3};

/// Turns on in `on` each of these extensions that `available` has.
pub fn extensions(available: &xr::ExtensionSet, on: &mut xr::ExtensionSet) {
    on.fb_render_model = available.fb_render_model;
    on.fb_hand_tracking_mesh = available.fb_hand_tracking_mesh && available.ext_hand_tracking;
}

/// The model's path for a controller in hand `side` (0 left, 1 right).
fn controller_path(side: usize) -> &'static str {
    if side == 0 {
        "/model_fb/controller/left"
    } else {
        "/model_fb/controller/right"
    }
}

/// The glTF binary of the controller now in hand `side`, with its model's name (for
/// the log) and key (the same key is the same model). `None`: the extension is off,
/// no controller is there, or the runtime has no model of it.
pub fn controller(session: &xr::Session<Gfx>, side: usize) -> Option<(u64, String, Vec<u8>)> {
    let ext = session.instance().exts().fb_render_model.as_ref()?;
    let path = session
        .instance()
        .string_to_path(controller_path(side))
        .ok()?;
    // Whether the runtime has models of this path at all (each it has is listed).
    let mut count = 0;
    // SAFETY: a live session; asking for the count with no room is the two-call idiom.
    let result = unsafe {
        (ext.enumerate_render_model_paths)(session.as_raw(), 0, &raw mut count, ptr::null_mut())
    };
    if result.into_raw() < 0 || count == 0 {
        return None;
    }
    let empty = xr::sys::RenderModelPathInfoFB {
        ty: xr::sys::RenderModelPathInfoFB::TYPE,
        next: ptr::null_mut(),
        path: xr::Path::NULL,
    };
    let mut paths = vec![empty; count as usize];
    // SAFETY: as above, `paths` has room for `count` typed entries.
    let result = unsafe {
        (ext.enumerate_render_model_paths)(
            session.as_raw(),
            count,
            &raw mut count,
            paths.as_mut_ptr(),
        )
    };
    if result.into_raw() < 0 || !paths.iter().take(count as usize).any(|p| p.path == path) {
        return None;
    }
    // Both glTF subsets the extension names: the reader takes several meshes and
    // pictures, and a picture it cannot decode gets one colour.
    let mut wanted = xr::sys::RenderModelCapabilitiesRequestFB {
        ty: xr::sys::RenderModelCapabilitiesRequestFB::TYPE,
        next: ptr::null_mut(),
        flags: xr::sys::RenderModelFlagsFB::SUPPORTS_GLTF_2_0_SUBSET_1
            | xr::sys::RenderModelFlagsFB::SUPPORTS_GLTF_2_0_SUBSET_2,
    };
    let mut props = xr::sys::RenderModelPropertiesFB {
        ty: xr::sys::RenderModelPropertiesFB::TYPE,
        next: (&raw mut wanted).cast(),
        vendor_id: 0,
        model_name: [0; xr::sys::MAX_RENDER_MODEL_NAME_SIZE_FB],
        model_key: xr::sys::RenderModelKeyFB::NULL,
        model_version: 0,
        flags: xr::sys::RenderModelFlagsFB::EMPTY,
    };
    // SAFETY: a live session, a path of this instance, and properties chaining a
    // typed request, both alive for the call.
    let result =
        unsafe { (ext.get_render_model_properties)(session.as_raw(), path, &raw mut props) };
    // Success but unavailable (no controller there now) leaves the key null.
    if result.into_raw() < 0 || props.model_key == xr::sys::RenderModelKeyFB::NULL {
        if result.into_raw() < 0 {
            warn!("controller model {}: {result:?}", controller_path(side));
        }
        return None;
    }
    // SAFETY: the runtime wrote a terminated name of at most its size.
    let name = unsafe { CStr::from_ptr(props.model_name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let info = xr::sys::RenderModelLoadInfoFB {
        ty: xr::sys::RenderModelLoadInfoFB::TYPE,
        next: ptr::null_mut(),
        model_key: props.model_key,
    };
    let mut buffer = xr::sys::RenderModelBufferFB {
        ty: xr::sys::RenderModelBufferFB::TYPE,
        next: ptr::null_mut(),
        buffer_capacity_input: 0,
        buffer_count_output: 0,
        buffer: ptr::null_mut(),
    };
    // SAFETY: a key the runtime just gave; no room yet, the size comes back.
    let result =
        unsafe { (ext.load_render_model)(session.as_raw(), &raw const info, &raw mut buffer) };
    if result.into_raw() < 0 || buffer.buffer_count_output == 0 {
        warn!("controller model {name}: {result:?}");
        return None;
    }
    let mut bytes = vec![0u8; buffer.buffer_count_output as usize];
    buffer.buffer_capacity_input = buffer.buffer_count_output;
    buffer.buffer = bytes.as_mut_ptr();
    // SAFETY: as above, `bytes` has room for the size the runtime said.
    let result =
        unsafe { (ext.load_render_model)(session.as_raw(), &raw const info, &raw mut buffer) };
    if result.into_raw() < 0 {
        warn!("controller model {name}: {result:?}");
        return None;
    }
    bytes.truncate(buffer.buffer_count_output as usize);
    info!(
        "controller model {name} for the {} hand: {} bytes",
        if side == 0 { "left" } else { "right" },
        bytes.len()
    );
    Some((props.model_key.into_raw(), name, bytes))
}

/// The mesh of hand `tracker`'s hand, checked (`None`: the extension is off, or the
/// runtime gave none or a broken one).
pub fn hand(instance: &xr::Instance, tracker: &xr::HandTracker) -> Option<HandMesh> {
    let ext = instance.exts().fb_hand_tracking_mesh.as_ref()?;
    let mut mesh = xr::sys::HandTrackingMeshFB {
        ty: xr::sys::HandTrackingMeshFB::TYPE,
        next: ptr::null_mut(),
        joint_capacity_input: 0,
        joint_count_output: 0,
        joint_bind_poses: ptr::null_mut(),
        joint_radii: ptr::null_mut(),
        joint_parents: ptr::null_mut(),
        vertex_capacity_input: 0,
        vertex_count_output: 0,
        vertex_positions: ptr::null_mut(),
        vertex_normals: ptr::null_mut(),
        vertex_u_vs: ptr::null_mut(),
        vertex_blend_indices: ptr::null_mut(),
        vertex_blend_weights: ptr::null_mut(),
        index_capacity_input: 0,
        index_count_output: 0,
        indices: ptr::null_mut(),
    };
    // SAFETY: a live tracker; no room yet, the counts come back.
    let result = unsafe { (ext.get_hand_mesh)(tracker.as_raw(), &raw mut mesh) };
    if result.into_raw() < 0 {
        warn!("hand mesh: {result:?}");
        return None;
    }
    let (joints, vertices, indices) = (
        mesh.joint_count_output as usize,
        mesh.vertex_count_output as usize,
        mesh.index_count_output as usize,
    );
    if joints != JOINTS || vertices == 0 || indices == 0 {
        warn!("hand mesh of {joints} joints, {vertices} corners, {indices} indices");
        return None;
    }
    let mut bind = vec![xr::Posef::IDENTITY; joints];
    let mut radii = vec![0.0f32; joints];
    let mut parents = vec![xr::HandJoint::PALM; joints];
    let zero3 = xr::Vector3f::default();
    let mut positions = vec![zero3; vertices];
    let mut normals = vec![zero3; vertices];
    let mut uvs = vec![xr::Vector2f::default(); vertices];
    let zero4s = xr::sys::Vector4sFB {
        x: 0,
        y: 0,
        z: 0,
        w: 0,
    };
    let mut blend = vec![zero4s; vertices];
    let mut weights = vec![xr::Vector4f::default(); vertices];
    let mut index = vec![0i16; indices];
    mesh.joint_capacity_input = joints as u32;
    mesh.joint_bind_poses = bind.as_mut_ptr();
    mesh.joint_radii = radii.as_mut_ptr();
    mesh.joint_parents = parents.as_mut_ptr();
    mesh.vertex_capacity_input = vertices as u32;
    mesh.vertex_positions = positions.as_mut_ptr();
    mesh.vertex_normals = normals.as_mut_ptr();
    mesh.vertex_u_vs = uvs.as_mut_ptr();
    mesh.vertex_blend_indices = blend.as_mut_ptr();
    mesh.vertex_blend_weights = weights.as_mut_ptr();
    mesh.index_capacity_input = indices as u32;
    mesh.indices = index.as_mut_ptr();
    // SAFETY: every array has room for the count the runtime said, and lives past
    // the call.
    let result = unsafe { (ext.get_hand_mesh)(tracker.as_raw(), &raw mut mesh) };
    if result.into_raw() < 0 {
        warn!("hand mesh: {result:?}");
        return None;
    }
    let v3 = |v: &xr::Vector3f| [v.x, v.y, v.z];
    let given = Given {
        positions: positions.iter().map(v3).collect(),
        normals: normals.iter().map(v3).collect(),
        joints: blend.iter().map(|b| [b.x, b.y, b.z, b.w]).collect(),
        weights: weights.iter().map(|w| [w.x, w.y, w.z, w.w]).collect(),
        indices: index,
        bind: bind
            .iter()
            .map(|p| (vec3(p.position), quat(p.orientation)))
            .collect(),
    };
    match HandMesh::new(given) {
        Ok(m) => {
            info!(
                "hand mesh: {} corners, {} triangles",
                m.positions.len(),
                m.triangles()
            );
            Some(m)
        }
        Err(e) => {
            warn!("hand mesh: {e}");
            None
        }
    }
}
