//! A tiny SPIR-V module builder, and the two shaders of the Vulkan backend.
//!
//! There is no shader compiler in the standard library, and a zero-dependency
//! crate cannot shell out to `glslc` at build time, so the shaders are
//! written directly as SPIR-V words. The builder knows only what those two
//! shaders need: sections in the order the SPIR-V spec's "Logical Layout of a
//! Module" (§2.4) requires, fresh result ids, de-duplicated types and
//! constants (§2.8: a non-aggregate type may be declared only once), and
//! NUL-terminated, word-padded literal strings (§2.2.1).
//!
//! Opcode and enumerant values are those of the SPIR-V headers
//! (`KhronosGroup/SPIRV-Headers`, `include/spirv/unified1/spirv.h`). Modules
//! are emitted as SPIR-V 1.0, the version every Vulkan 1.0 driver accepts.
//!
//! # The pixel rule on the GPU
//!
//! The fragment shader reproduces the crate's pixel rule **in integers**: the
//! coverage texel is turned back into its byte (`a = uint(r * 255 + 0.5)`),
//! each channel is `(bg * (255 - a) + fg * a + 127) / 255` with `OpUDiv`, and
//! the result is written as `channel / 255.0` to a `UNORM` target, whose
//! float-to-unorm conversion returns the same byte. The only float math on the
//! colour path is the exact `k / 255` round trip.

use std::collections::HashMap;

/// SPIR-V magic number.
pub const MAGIC: u32 = 0x0723_0203;
/// SPIR-V 1.0, the version word of every module this builder emits.
pub const VERSION_1_0: u32 = 0x0001_0000;

/// Opcodes used here (`SpvOp*` in `spirv.h`).
#[allow(missing_docs)]
pub mod op {
    pub const MEMORY_MODEL: u16 = 14;
    pub const ENTRY_POINT: u16 = 15;
    pub const EXECUTION_MODE: u16 = 16;
    pub const CAPABILITY: u16 = 17;
    pub const TYPE_VOID: u16 = 19;
    pub const TYPE_BOOL: u16 = 20;
    pub const TYPE_INT: u16 = 21;
    pub const TYPE_FLOAT: u16 = 22;
    pub const TYPE_VECTOR: u16 = 23;
    pub const TYPE_IMAGE: u16 = 25;
    pub const TYPE_STRUCT: u16 = 30;
    pub const TYPE_POINTER: u16 = 32;
    pub const TYPE_FUNCTION: u16 = 33;
    pub const CONSTANT: u16 = 43;
    pub const CONSTANT_COMPOSITE: u16 = 44;
    pub const FUNCTION: u16 = 54;
    pub const FUNCTION_END: u16 = 56;
    pub const VARIABLE: u16 = 59;
    pub const LOAD: u16 = 61;
    pub const STORE: u16 = 62;
    pub const ACCESS_CHAIN: u16 = 65;
    pub const DECORATE: u16 = 71;
    pub const MEMBER_DECORATE: u16 = 72;
    pub const COMPOSITE_CONSTRUCT: u16 = 80;
    pub const COMPOSITE_EXTRACT: u16 = 81;
    pub const IMAGE_FETCH: u16 = 95;
    pub const CONVERT_F_TO_U: u16 = 109;
    pub const CONVERT_U_TO_F: u16 = 112;
    pub const BITCAST: u16 = 124;
    pub const I_ADD: u16 = 128;
    pub const F_ADD: u16 = 129;
    pub const I_SUB: u16 = 130;
    pub const F_SUB: u16 = 131;
    pub const I_MUL: u16 = 132;
    pub const F_MUL: u16 = 133;
    pub const U_DIV: u16 = 134;
    pub const F_DIV: u16 = 136;
    pub const U_MOD: u16 = 137;
    pub const LOGICAL_OR: u16 = 166;
    pub const LOGICAL_AND: u16 = 167;
    pub const SELECT: u16 = 169;
    pub const I_EQUAL: u16 = 170;
    pub const I_NOT_EQUAL: u16 = 171;
    pub const U_GREATER_THAN_EQUAL: u16 = 174;
    pub const SHIFT_RIGHT_LOGICAL: u16 = 194;
    pub const BITWISE_AND: u16 = 199;
    pub const LABEL: u16 = 248;
    pub const RETURN: u16 = 253;
}

// Enumerants (`spirv.h`).
const CAPABILITY_SHADER: u32 = 1;
const ADDRESSING_LOGICAL: u32 = 0;
const MEMORY_GLSL450: u32 = 1;
const EXEC_VERTEX: u32 = 0;
const EXEC_FRAGMENT: u32 = 4;
const MODE_ORIGIN_UPPER_LEFT: u32 = 7;
const DEC_BLOCK: u32 = 2;
const DEC_BUILTIN: u32 = 11;
const DEC_FLAT: u32 = 14;
const DEC_LOCATION: u32 = 30;
const DEC_BINDING: u32 = 33;
const DEC_DESCRIPTOR_SET: u32 = 34;
const DEC_OFFSET: u32 = 35;
const BUILTIN_POSITION: u32 = 0;
const BUILTIN_FRAG_COORD: u32 = 15;
const BUILTIN_VERTEX_INDEX: u32 = 42;
const BUILTIN_INSTANCE_INDEX: u32 = 43;
const SC_UNIFORM_CONSTANT: u32 = 0;
const SC_INPUT: u32 = 1;
const SC_OUTPUT: u32 = 3;
const SC_PUSH_CONSTANT: u32 = 9;
const DIM_2D: u32 = 1;
const IMAGE_FORMAT_UNKNOWN: u32 = 0;
const IMAGE_OPERANDS_LOD: u32 = 0x2;
const FUNCTION_CONTROL_NONE: u32 = 0;

/// A SPIR-V module under construction.
#[derive(Debug, Default)]
pub struct Module {
    next_id: u32,
    capabilities: Vec<u32>,
    memory_model: Vec<u32>,
    entry_points: Vec<u32>,
    execution_modes: Vec<u32>,
    annotations: Vec<u32>,
    globals: Vec<u32>,
    functions: Vec<u32>,
    /// Types and constants already declared: `(opcode, operands)` to id.
    cache: HashMap<(u16, Vec<u32>), u32>,
}

/// Append one instruction: the first word is `word_count << 16 | opcode`.
fn emit(out: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
    let count = operands.len() + 1;
    assert!(count <= 0xFFFF, "instruction too long");
    out.push((count as u32) << 16 | opcode as u32);
    out.extend_from_slice(operands);
}

/// A literal string: UTF-8, NUL-terminated, zero-padded to whole words,
/// little-endian within each word (SPIR-V spec §2.2.1).
pub fn string_words(s: &str) -> Vec<u32> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    bytes
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl Module {
    /// An empty module declaring `Shader` and the `Logical GLSL450` model.
    pub fn new() -> Module {
        let mut m = Module {
            next_id: 1,
            ..Module::default()
        };
        emit(&mut m.capabilities, op::CAPABILITY, &[CAPABILITY_SHADER]);
        emit(
            &mut m.memory_model,
            op::MEMORY_MODEL,
            &[ADDRESSING_LOGICAL, MEMORY_GLSL450],
        );
        m
    }

    /// A fresh result id.
    pub fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Declare a type or constant once; a repeat returns the first id.
    /// `operands` excludes the result id (and, for constants, starts with
    /// the result type).
    fn cached(&mut self, opcode: u16, operands: &[u32], constant: bool) -> u32 {
        if let Some(&id) = self.cache.get(&(opcode, operands.to_vec())) {
            return id;
        }
        let id = self.id();
        let mut words = Vec::with_capacity(operands.len() + 1);
        if constant {
            // OpConstant*: <result type> <result id> <values...>
            words.push(operands[0]);
            words.push(id);
            words.extend_from_slice(&operands[1..]);
        } else {
            // OpType*: <result id> <operands...>
            words.push(id);
            words.extend_from_slice(operands);
        }
        emit(&mut self.globals, opcode, &words);
        self.cache.insert((opcode, operands.to_vec()), id);
        id
    }

    /// `void`.
    pub fn t_void(&mut self) -> u32 {
        self.cached(op::TYPE_VOID, &[], false)
    }
    /// `bool`.
    pub fn t_bool(&mut self) -> u32 {
        self.cached(op::TYPE_BOOL, &[], false)
    }
    /// 32-bit unsigned integer.
    pub fn t_u32(&mut self) -> u32 {
        self.cached(op::TYPE_INT, &[32, 0], false)
    }
    /// 32-bit signed integer.
    pub fn t_i32(&mut self) -> u32 {
        self.cached(op::TYPE_INT, &[32, 1], false)
    }
    /// 32-bit float.
    pub fn t_f32(&mut self) -> u32 {
        self.cached(op::TYPE_FLOAT, &[32], false)
    }
    /// A vector of `n` components of `elem`.
    pub fn t_vec(&mut self, elem: u32, n: u32) -> u32 {
        self.cached(op::TYPE_VECTOR, &[elem, n], false)
    }
    /// A pointer to `ty` in `storage`.
    pub fn t_ptr(&mut self, storage: u32, ty: u32) -> u32 {
        self.cached(op::TYPE_POINTER, &[storage, ty], false)
    }
    /// A function type returning `ret` and taking no parameters.
    pub fn t_fn(&mut self, ret: u32) -> u32 {
        self.cached(op::TYPE_FUNCTION, &[ret], false)
    }
    /// A sampled 2D float image (`texture2D`), no depth, not arrayed, not
    /// multisampled, used with a sampler or fetch (`Sampled = 1`).
    pub fn t_image_2d(&mut self) -> u32 {
        let f = self.t_f32();
        // OpTypeImage: Sampled Type, Dim, Depth, Arrayed, MS, Sampled, Format.
        self.cached(
            op::TYPE_IMAGE,
            &[f, DIM_2D, 0, 0, 0, 1, IMAGE_FORMAT_UNKNOWN],
            false,
        )
    }
    /// A struct of `members`. Never de-duplicated: two structs may carry
    /// different decorations.
    pub fn t_struct(&mut self, members: &[u32]) -> u32 {
        let id = self.id();
        let mut w = vec![id];
        w.extend_from_slice(members);
        emit(&mut self.globals, op::TYPE_STRUCT, &w);
        id
    }

    /// An unsigned 32-bit constant.
    pub fn c_u32(&mut self, v: u32) -> u32 {
        let t = self.t_u32();
        self.cached(op::CONSTANT, &[t, v], true)
    }
    /// A signed 32-bit constant.
    pub fn c_i32(&mut self, v: i32) -> u32 {
        let t = self.t_i32();
        self.cached(op::CONSTANT, &[t, v as u32], true)
    }
    /// A 32-bit float constant.
    pub fn c_f32(&mut self, v: f32) -> u32 {
        let t = self.t_f32();
        self.cached(op::CONSTANT, &[t, v.to_bits()], true)
    }
    /// A composite constant of type `ty` from constant `parts`.
    pub fn c_composite(&mut self, ty: u32, parts: &[u32]) -> u32 {
        let mut w = vec![ty];
        w.extend_from_slice(parts);
        self.cached(op::CONSTANT_COMPOSITE, &w, true)
    }

    /// A module-scope variable of pointer type `ptr` in `storage`.
    pub fn variable(&mut self, ptr: u32, storage: u32) -> u32 {
        let id = self.id();
        emit(&mut self.globals, op::VARIABLE, &[ptr, id, storage]);
        id
    }

    /// `OpDecorate target decoration extra...`.
    pub fn decorate(&mut self, target: u32, decoration: u32, extra: &[u32]) {
        let mut w = vec![target, decoration];
        w.extend_from_slice(extra);
        emit(&mut self.annotations, op::DECORATE, &w);
    }

    /// `OpMemberDecorate struct member decoration extra...`.
    pub fn member_decorate(&mut self, st: u32, member: u32, decoration: u32, extra: &[u32]) {
        let mut w = vec![st, member, decoration];
        w.extend_from_slice(extra);
        emit(&mut self.annotations, op::MEMBER_DECORATE, &w);
    }

    /// `OpEntryPoint model function "name" interface...`.
    pub fn entry_point(&mut self, model: u32, function: u32, name: &str, interface: &[u32]) {
        let mut w = vec![model, function];
        w.extend(string_words(name));
        w.extend_from_slice(interface);
        emit(&mut self.entry_points, op::ENTRY_POINT, &w);
    }

    /// `OpExecutionMode function mode`.
    pub fn execution_mode(&mut self, function: u32, mode: u32) {
        emit(
            &mut self.execution_modes,
            op::EXECUTION_MODE,
            &[function, mode],
        );
    }

    /// Open a `void main()` function and its entry block; returns its id.
    pub fn begin_main(&mut self) -> u32 {
        let void = self.t_void();
        let fn_ty = self.t_fn(void);
        let f = self.id();
        emit(
            &mut self.functions,
            op::FUNCTION,
            &[void, f, FUNCTION_CONTROL_NONE, fn_ty],
        );
        let label = self.id();
        emit(&mut self.functions, op::LABEL, &[label]);
        f
    }

    /// Close the function opened by [`Module::begin_main`].
    pub fn end_main(&mut self) {
        emit(&mut self.functions, op::RETURN, &[]);
        emit(&mut self.functions, op::FUNCTION_END, &[]);
    }

    /// A function-body instruction with a result: `opcode ty id operands...`.
    pub fn op(&mut self, opcode: u16, ty: u32, operands: &[u32]) -> u32 {
        let id = self.id();
        let mut w = vec![ty, id];
        w.extend_from_slice(operands);
        emit(&mut self.functions, opcode, &w);
        id
    }

    /// A function-body instruction without a result.
    pub fn op_void(&mut self, opcode: u16, operands: &[u32]) {
        emit(&mut self.functions, opcode, operands);
    }

    /// The finished module: header, then every section in layout order.
    pub fn finish(self) -> Vec<u32> {
        // Header (§2.3): magic, version, generator (0 = unregistered), id
        // bound (every id is below it), schema (0).
        let mut out = vec![MAGIC, VERSION_1_0, 0, self.next_id, 0];
        for s in [
            &self.capabilities,
            &self.memory_model,
            &self.entry_points,
            &self.execution_modes,
            &self.annotations,
            &self.globals,
            &self.functions,
        ] {
            out.extend_from_slice(s);
        }
        out
    }
}

/// Push constants shared by both stages: six `u32`s, 24 bytes, in this order.
/// The Vulkan backend's `vkCmdPushConstants` writes exactly this layout.
pub const PUSH_CONSTANT_BYTES: u32 = 24;

/// Declare the push-constant block `{ cols, cell_w, cell_h, slots_per_row,
/// viewport_w, viewport_h }` (all `uint`, offsets 0..20); returns the variable.
fn push_constants(m: &mut Module) -> u32 {
    let u = m.t_u32();
    let st = m.t_struct(&[u; 6]);
    m.decorate(st, DEC_BLOCK, &[]);
    for i in 0..6 {
        m.member_decorate(st, i, DEC_OFFSET, &[i * 4]);
    }
    let ptr = m.t_ptr(SC_PUSH_CONSTANT, st);
    m.variable(ptr, SC_PUSH_CONSTANT)
}

/// Load push-constant member `index` (a `uint`).
fn load_pc(m: &mut Module, pc: u32, index: i32) -> u32 {
    let u = m.t_u32();
    let ptr = m.t_ptr(SC_PUSH_CONSTANT, u);
    // Struct indices must be OpConstant integers (spec, OpAccessChain).
    let idx = m.c_i32(index);
    let p = m.op(op::ACCESS_CHAIN, ptr, &[pc, idx]);
    m.op(op::LOAD, u, &[p])
}

/// The vertex shader.
///
/// One instance per cell, six vertices per instance (two triangles, list
/// topology). Inputs: location 0 = the cell's `uvec4 (glyph, fg, bg, flags)`
/// at instance rate; `gl_VertexIndex`, `gl_InstanceIndex`. The cell is
/// `(row, col) = (instance / cols, instance % cols)`, its pixel rectangle
/// `(col * cell_w, row * cell_h)` plus `(cell_w, cell_h)`, mapped to clip
/// space over `viewport_w x viewport_h`. Flat outputs: location 0 =
/// `(cell_x, cell_y, slot_x, slot_y)` in pixels, location 1 =
/// `(fg, bg, flags, 0)`.
pub fn vertex_shader() -> Vec<u32> {
    let mut m = Module::new();
    let u = m.t_u32();
    let i = m.t_i32();
    let f = m.t_f32();
    let uvec4 = m.t_vec(u, 4);
    let vec4 = m.t_vec(f, 4);

    let pc = push_constants(&mut m);
    let p_in_uvec4 = m.t_ptr(SC_INPUT, uvec4);
    let p_in_i32 = m.t_ptr(SC_INPUT, i);
    let p_out_vec4 = m.t_ptr(SC_OUTPUT, vec4);
    let p_out_uvec4 = m.t_ptr(SC_OUTPUT, uvec4);

    let inst = m.variable(p_in_uvec4, SC_INPUT);
    m.decorate(inst, DEC_LOCATION, &[0]);
    let vid = m.variable(p_in_i32, SC_INPUT);
    m.decorate(vid, DEC_BUILTIN, &[BUILTIN_VERTEX_INDEX]);
    let iid = m.variable(p_in_i32, SC_INPUT);
    m.decorate(iid, DEC_BUILTIN, &[BUILTIN_INSTANCE_INDEX]);
    let pos = m.variable(p_out_vec4, SC_OUTPUT);
    m.decorate(pos, DEC_BUILTIN, &[BUILTIN_POSITION]);
    let out_cell = m.variable(p_out_uvec4, SC_OUTPUT);
    m.decorate(out_cell, DEC_LOCATION, &[0]);
    m.decorate(out_cell, DEC_FLAT, &[]);
    let out_col = m.variable(p_out_uvec4, SC_OUTPUT);
    m.decorate(out_col, DEC_LOCATION, &[1]);
    m.decorate(out_col, DEC_FLAT, &[]);

    // Constants, declared before the body (they live in the globals section
    // regardless, so order here is only for readability).
    let c0 = m.c_u32(0);
    let c1 = m.c_u32(1);
    // Corner of each of the 6 vertices, as bit `vi` of a mask:
    //   vi:  0 1 2 3 4 5     triangles (0,1,2) and (3,4,5)
    //   x:   0 1 0 0 1 1  -> 0b110010 = 0x32
    //   y:   0 0 1 1 0 1  -> 0b101100 = 0x2C
    let mask_x = m.c_u32(0x32);
    let mask_y = m.c_u32(0x2C);
    let f0 = m.c_f32(0.0);
    let f1 = m.c_f32(1.0);
    let f2 = m.c_f32(2.0);

    let main = m.begin_main();
    let vi = m.op(op::LOAD, i, &[vid]);
    let vi = m.op(op::BITCAST, u, &[vi]);
    let ii = m.op(op::LOAD, i, &[iid]);
    let ii = m.op(op::BITCAST, u, &[ii]);
    let cols = load_pc(&mut m, pc, 0);
    let cw = load_pc(&mut m, pc, 1);
    let ch = load_pc(&mut m, pc, 2);
    let spr = load_pc(&mut m, pc, 3);
    let vw = load_pc(&mut m, pc, 4);
    let vh = load_pc(&mut m, pc, 5);

    let row = m.op(op::U_DIV, u, &[ii, cols]);
    let col = m.op(op::U_MOD, u, &[ii, cols]);
    let cx = m.op(op::I_MUL, u, &[col, cw]);
    let cy = m.op(op::I_MUL, u, &[row, ch]);
    let bx = m.op(op::SHIFT_RIGHT_LOGICAL, u, &[mask_x, vi]);
    let bx = m.op(op::BITWISE_AND, u, &[bx, c1]);
    let by = m.op(op::SHIFT_RIGHT_LOGICAL, u, &[mask_y, vi]);
    let by = m.op(op::BITWISE_AND, u, &[by, c1]);
    let dx = m.op(op::I_MUL, u, &[bx, cw]);
    let dy = m.op(op::I_MUL, u, &[by, ch]);
    let px = m.op(op::I_ADD, u, &[cx, dx]);
    let py = m.op(op::I_ADD, u, &[cy, dy]);

    // ndc = px / viewport * 2 - 1; Vulkan's clip space has +y down, so the
    // top row of cells lands at the top of the target.
    let ndc = |m: &mut Module, p: u32, extent: u32| {
        let pf = m.op(op::CONVERT_U_TO_F, f, &[p]);
        let ef = m.op(op::CONVERT_U_TO_F, f, &[extent]);
        let t = m.op(op::F_DIV, f, &[pf, ef]);
        let t = m.op(op::F_MUL, f, &[t, f2]);
        m.op(op::F_SUB, f, &[t, f1])
    };
    let nx = ndc(&mut m, px, vw);
    let ny = ndc(&mut m, py, vh);
    let position = m.op(op::COMPOSITE_CONSTRUCT, vec4, &[nx, ny, f0, f1]);
    m.op_void(op::STORE, &[pos, position]);

    let cell = m.op(op::LOAD, uvec4, &[inst]);
    let glyph = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 0]);
    let fg = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 1]);
    let bg = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 2]);
    let flags = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 3]);
    let sx = m.op(op::U_MOD, u, &[glyph, spr]);
    let sx = m.op(op::I_MUL, u, &[sx, cw]);
    let sy = m.op(op::U_DIV, u, &[glyph, spr]);
    let sy = m.op(op::I_MUL, u, &[sy, ch]);
    let o0 = m.op(op::COMPOSITE_CONSTRUCT, uvec4, &[cx, cy, sx, sy]);
    m.op_void(op::STORE, &[out_cell, o0]);
    let o1 = m.op(op::COMPOSITE_CONSTRUCT, uvec4, &[fg, bg, flags, c0]);
    m.op_void(op::STORE, &[out_col, o1]);
    m.end_main();

    m.entry_point(
        EXEC_VERTEX,
        main,
        "main",
        &[inst, vid, iid, pos, out_cell, out_col],
    );
    m.finish()
}

/// The fragment shader.
///
/// `local = uvec2(gl_FragCoord.xy) - cell_origin` (`FragCoord` is the pixel
/// centre, `n + 0.5`, so the conversion truncates to `n`); coverage is
/// `texelFetch(atlas, slot_origin + local, 0).r` from the `R8_UNORM` atlas
/// bound as a sampled image at set 0, binding 0 (no sampler, no filtering);
/// underline and strike force it to 255; the colour is the integer pixel rule.
pub fn fragment_shader() -> Vec<u32> {
    let mut m = Module::new();
    let u = m.t_u32();
    let i = m.t_i32();
    let f = m.t_f32();
    let b = m.t_bool();
    let uvec2 = m.t_vec(u, 2);
    let uvec3 = m.t_vec(u, 3);
    let uvec4 = m.t_vec(u, 4);
    let ivec2 = m.t_vec(i, 2);
    let vec3 = m.t_vec(f, 3);
    let vec4 = m.t_vec(f, 4);
    let image = m.t_image_2d();

    let pc = push_constants(&mut m);
    let p_in_vec4 = m.t_ptr(SC_INPUT, vec4);
    let p_in_uvec4 = m.t_ptr(SC_INPUT, uvec4);
    let p_out_vec4 = m.t_ptr(SC_OUTPUT, vec4);
    let p_image = m.t_ptr(SC_UNIFORM_CONSTANT, image);

    let frag_coord = m.variable(p_in_vec4, SC_INPUT);
    m.decorate(frag_coord, DEC_BUILTIN, &[BUILTIN_FRAG_COORD]);
    let in_cell = m.variable(p_in_uvec4, SC_INPUT);
    m.decorate(in_cell, DEC_LOCATION, &[0]);
    m.decorate(in_cell, DEC_FLAT, &[]);
    let in_col = m.variable(p_in_uvec4, SC_INPUT);
    m.decorate(in_col, DEC_LOCATION, &[1]);
    m.decorate(in_col, DEC_FLAT, &[]);
    let atlas = m.variable(p_image, SC_UNIFORM_CONSTANT);
    m.decorate(atlas, DEC_DESCRIPTOR_SET, &[0]);
    m.decorate(atlas, DEC_BINDING, &[0]);
    let out = m.variable(p_out_vec4, SC_OUTPUT);
    m.decorate(out, DEC_LOCATION, &[0]);

    let c0 = m.c_u32(0);
    let c1 = m.c_u32(1);
    let c2 = m.c_u32(2);
    let c3 = m.c_u32(3);
    let c8 = m.c_u32(8);
    let c16 = m.c_u32(16);
    let c127 = m.c_u32(127);
    let c255 = m.c_u32(255);
    let i0 = m.c_i32(0);
    let f_half = m.c_f32(0.5);
    let f1 = m.c_f32(1.0);
    let f255 = m.c_f32(255.0);
    let shifts = m.c_composite(uvec3, &[c16, c8, c0]);
    let v127 = m.c_composite(uvec3, &[c127, c127, c127]);
    let v255 = m.c_composite(uvec3, &[c255, c255, c255]);
    let vf255 = m.c_composite(vec3, &[f255, f255, f255]);

    let main = m.begin_main();
    let fc = m.op(op::LOAD, vec4, &[frag_coord]);
    let fx = m.op(op::COMPOSITE_EXTRACT, f, &[fc, 0]);
    let fy = m.op(op::COMPOSITE_EXTRACT, f, &[fc, 1]);
    let px = m.op(op::CONVERT_F_TO_U, u, &[fx]);
    let py = m.op(op::CONVERT_F_TO_U, u, &[fy]);
    let cell = m.op(op::LOAD, uvec4, &[in_cell]);
    let cx = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 0]);
    let cy = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 1]);
    let sx = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 2]);
    let sy = m.op(op::COMPOSITE_EXTRACT, u, &[cell, 3]);
    let lx = m.op(op::I_SUB, u, &[px, cx]);
    let ly = m.op(op::I_SUB, u, &[py, cy]);
    let tx = m.op(op::I_ADD, u, &[sx, lx]);
    let ty = m.op(op::I_ADD, u, &[sy, ly]);
    let tc = m.op(op::COMPOSITE_CONSTRUCT, uvec2, &[tx, ty]);
    let tc = m.op(op::BITCAST, ivec2, &[tc]);
    let img = m.op(op::LOAD, image, &[atlas]);
    // OpImageFetch <image> <coordinate> Lod <lod>: Vulkan wants an explicit
    // Lod on a non-multisampled fetch.
    let texel = m.op(op::IMAGE_FETCH, vec4, &[img, tc, IMAGE_OPERANDS_LOD, i0]);
    let r = m.op(op::COMPOSITE_EXTRACT, f, &[texel, 0]);
    let r = m.op(op::F_MUL, f, &[r, f255]);
    let r = m.op(op::F_ADD, f, &[r, f_half]);
    let a = m.op(op::CONVERT_F_TO_U, u, &[r]);

    let col = m.op(op::LOAD, uvec4, &[in_col]);
    let fg = m.op(op::COMPOSITE_EXTRACT, u, &[col, 0]);
    let bg = m.op(op::COMPOSITE_EXTRACT, u, &[col, 1]);
    let flags = m.op(op::COMPOSITE_EXTRACT, u, &[col, 2]);
    let ch = load_pc(&mut m, pc, 2);

    // underline: (flags & 1) != 0 && cell_h >= 3 && ly == cell_h - 2
    let fu = m.op(op::BITWISE_AND, u, &[flags, c1]);
    let fu = m.op(op::I_NOT_EQUAL, b, &[fu, c0]);
    let tall = m.op(op::U_GREATER_THAN_EQUAL, b, &[ch, c3]);
    let ul_row = m.op(op::I_SUB, u, &[ch, c2]);
    let at_ul = m.op(op::I_EQUAL, b, &[ly, ul_row]);
    let ul = m.op(op::LOGICAL_AND, b, &[fu, tall]);
    let ul = m.op(op::LOGICAL_AND, b, &[ul, at_ul]);
    // strike: (flags & 2) != 0 && ly == cell_h / 2
    let fs = m.op(op::BITWISE_AND, u, &[flags, c2]);
    let fs = m.op(op::I_NOT_EQUAL, b, &[fs, c0]);
    let st_row = m.op(op::U_DIV, u, &[ch, c2]);
    let at_st = m.op(op::I_EQUAL, b, &[ly, st_row]);
    let st = m.op(op::LOGICAL_AND, b, &[fs, at_st]);
    let line = m.op(op::LOGICAL_OR, b, &[ul, st]);
    let a = m.op(op::SELECT, u, &[line, c255, a]);

    // Channels (r, g, b) = (c >> 16, c >> 8, c) & 255, as uvec3.
    let channels = |m: &mut Module, c: u32| {
        let v = m.op(op::COMPOSITE_CONSTRUCT, uvec3, &[c, c, c]);
        let v = m.op(op::SHIFT_RIGHT_LOGICAL, uvec3, &[v, shifts]);
        m.op(op::BITWISE_AND, uvec3, &[v, v255])
    };
    let fgv = channels(&mut m, fg);
    let bgv = channels(&mut m, bg);
    let av = m.op(op::COMPOSITE_CONSTRUCT, uvec3, &[a, a, a]);
    let inv = m.op(op::I_SUB, uvec3, &[v255, av]);
    let t0 = m.op(op::I_MUL, uvec3, &[bgv, inv]);
    let t1 = m.op(op::I_MUL, uvec3, &[fgv, av]);
    let sum = m.op(op::I_ADD, uvec3, &[t0, t1]);
    let sum = m.op(op::I_ADD, uvec3, &[sum, v127]);
    let q = m.op(op::U_DIV, uvec3, &[sum, v255]);
    let qf = m.op(op::CONVERT_U_TO_F, vec3, &[q]);
    let rgb = m.op(op::F_DIV, vec3, &[qf, vf255]);
    let color = m.op(op::COMPOSITE_CONSTRUCT, vec4, &[rgb, f1]);
    m.op_void(op::STORE, &[out, color]);
    m.end_main();

    m.entry_point(
        EXEC_FRAGMENT,
        main,
        "main",
        &[frag_coord, in_cell, in_col, out],
    );
    m.execution_mode(main, MODE_ORIGIN_UPPER_LEFT);
    m.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Where an opcode's result id sits, if it has one: word 1 for types and
    /// labels, word 2 (after the result type) for everything else that
    /// produces a value.
    fn result_word(opcode: u16) -> Option<usize> {
        match opcode {
            op::TYPE_VOID..=op::TYPE_FUNCTION | op::LABEL => Some(1),
            op::CONSTANT
            | op::CONSTANT_COMPOSITE
            | op::FUNCTION
            | op::VARIABLE
            | op::LOAD
            | op::ACCESS_CHAIN
            | op::COMPOSITE_CONSTRUCT
            | op::COMPOSITE_EXTRACT
            | op::IMAGE_FETCH
            | op::CONVERT_F_TO_U
            | op::CONVERT_U_TO_F
            | op::BITCAST
            | op::I_ADD..=op::U_MOD
            | op::LOGICAL_OR..=op::U_GREATER_THAN_EQUAL
            | op::SHIFT_RIGHT_LOGICAL
            | op::BITWISE_AND => Some(2),
            _ => None,
        }
    }

    /// Walk a module: every instruction's word count must land exactly on the
    /// next instruction, and every result id must be unique and below the
    /// bound, with the bound tight (every id below it defined).
    fn walk(words: &[u32]) -> Vec<u16> {
        assert_eq!(words[0], MAGIC, "magic");
        assert_eq!(words[1], VERSION_1_0, "version");
        assert_eq!(words[4], 0, "schema");
        let bound = words[3];
        let mut defined = HashSet::new();
        let mut opcodes = Vec::new();
        let mut at = 5;
        while at < words.len() {
            let count = (words[at] >> 16) as usize;
            let opcode = (words[at] & 0xFFFF) as u16;
            assert!(count >= 1, "zero word count at word {at}");
            assert!(
                at + count <= words.len(),
                "instruction at word {at} (op {opcode}) runs past the end"
            );
            if let Some(w) = result_word(opcode) {
                let id = words[at + w];
                assert!(id != 0 && id < bound, "id {id} outside bound {bound}");
                assert!(defined.insert(id), "id {id} defined twice");
            }
            opcodes.push(opcode);
            at += count;
        }
        assert_eq!(at, words.len());
        assert_eq!(defined.len() as u32, bound - 1, "bound is not tight");
        opcodes
    }

    #[test]
    fn strings_are_nul_terminated_and_padded() {
        assert_eq!(string_words("main"), vec![0x6E69_616D, 0]);
        assert_eq!(string_words("abc"), vec![0x0063_6261]);
        assert_eq!(string_words(""), vec![0]);
    }

    #[test]
    fn types_and_constants_are_declared_once() {
        let mut m = Module::new();
        let a = m.t_u32();
        let b = m.t_u32();
        assert_eq!(a, b);
        assert_eq!(m.c_u32(7), m.c_u32(7));
        assert_ne!(m.c_u32(7), m.c_i32(7));
        let words = m.finish();
        walk(&words);
    }

    #[test]
    fn the_vertex_shader_is_well_formed() {
        let v = vertex_shader();
        let ops = walk(&v);
        assert_eq!(ops[0], op::CAPABILITY);
        assert_eq!(ops[1], op::MEMORY_MODEL);
        assert_eq!(ops[2], op::ENTRY_POINT);
        assert_eq!(*ops.last().unwrap(), op::FUNCTION_END);
    }

    #[test]
    fn the_fragment_shader_is_well_formed() {
        let f = fragment_shader();
        let ops = walk(&f);
        assert_eq!(
            &ops[..4],
            &[
                op::CAPABILITY,
                op::MEMORY_MODEL,
                op::ENTRY_POINT,
                op::EXECUTION_MODE
            ]
        );
        assert!(ops.contains(&op::IMAGE_FETCH));
        assert_eq!(*ops.last().unwrap(), op::FUNCTION_END);
    }

    #[test]
    fn sections_follow_the_logical_layout() {
        // Annotations precede types, types precede the function.
        for words in [vertex_shader(), fragment_shader()] {
            let ops = walk(&words);
            let last_dec = ops.iter().rposition(|&o| o == op::DECORATE).unwrap();
            let first_type = ops
                .iter()
                .position(|&o| o == op::TYPE_VOID || o == op::TYPE_INT);
            let first_fn = ops.iter().position(|&o| o == op::FUNCTION).unwrap();
            assert!(last_dec < first_type.unwrap());
            assert!(first_type.unwrap() < first_fn);
        }
    }
}
