//! Dathinel (561503d) and PiloteCode (#854) synthetic guest and harness.
//! Includes the bounded-EXEC and guarded-memory variant from NEW-354.

use super::host::{Dispatch, Host, Root, View};
use super::validator;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{Container, FourCC, dxbc::u32_at};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

const EXEC: u32 = 126;
const VCC: u32 = 106;
const GUARD: usize = 8;
const OUTPUTS: usize = 7;

const fn k(value: u32) -> u32 {
    128 + value
}
const fn v(reg: u32) -> u32 {
    256 + reg
}
const fn mov(dst: u32, src: u32) -> u32 {
    0x7E00_0200 | dst << 17 | src
}
const fn sop1(op: u32, dst: u32, src: u32) -> u32 {
    0xBE80_0000 | dst << 16 | op << 8 | src
}
const fn alu(op: u32, dst: u32, left: u32, right: u32) -> u32 {
    op << 25 | dst << 17 | right << 9 | left
}
const fn cmp(op: u32, left: u32, right: u32) -> u32 {
    0x7C00_0000 | op << 17 | right << 9 | left
}
const fn ds(op: u32, dst: u32, address: u32, data: u32) -> [u32; 2] {
    [0xD800_0000 | op << 18, dst << 24 | data << 8 | address]
}

fn guest(threads: u32) -> NativeProgram {
    let mut words = vec![
        alu(0x16, 1, k(6), 0),  // v1: logical wave index
        alu(0x1B, 2, k(63), 0), // v2: logical lane index
        alu(0x1A, 11, k(2), 1), // v11: byte address of wave's LDS counter
        mov(12, k(0)),
        sop1(4, 22, EXEC),
        cmp(0xD2, k(0), 2), // only logical lane 0 initializes this wave's counter
    ];
    words.extend(ds(0x0D, 0, 11, 12));
    words.extend([
        sop1(4, EXEC, 22),
        alu(0x1B, 3, k(3), 2),
        alu(0x25, 3, k(1), 3),
        alu(0x25, 3, v(1), 3), // rounds = 1 + (lane & 3) + wave
        mov(4, k(if threads == 96 { 11 } else { 0 })),
        mov(13, k(1)),
        sop1(4, 20, EXEC),
        alu(0x25, 4, v(2), 4), // loop: accumulate the lane number
    ]);
    words.extend(ds(0, 0, 11, 13)); // ds_add_u32: add one per participating lane
    words.extend([
        alu(0x25, 3, 193, 3), // decrement remaining rounds
        cmp(0xC5, k(0), 3),
        0x8000_0000 | 0x0F << 23 | EXEC << 16 | VCC << 8 | EXEC, // s_and_b64 exec, exec, vcc
        0xBF89_FFF9,                                             // s_cbranch_execnz loop
        sop1(4, EXEC, 20),
    ]);
    words.extend(ds(0x36, 5, 11, 0)); // v5: total from own wave

    // Wave 0 first reads lane 40 and then lane 20. Wave 1 selects lane 24
    // and then lane 36, so the same instruction selects opposite halves.
    // Alternate the elected half by wave parity for groups with four waves.
    let selector = if threads == 256 {
        words.push(alu(0x1B, 8, k(1), 1)); // v8 = wave index & 1
        8
    } else {
        1
    };
    for (threshold, op, scalar, dest) in [(39, 0x26, 6, 6), (19, 0x25, 7, 9)] {
        words.extend([
            alu(0x1A, 14, k(4), selector),
            alu(op, 14, k(threshold), 14),
            sop1(4, 24, EXEC),
            cmp(0xD1, v(14), 2),               // v_cmpx_lt_u32 threshold, lane
            0x7E00_0400 | scalar << 17 | v(4), // v_readfirstlane_b32
            sop1(4, EXEC, 24),
            mov(dest, scalar),
        ]);
    }

    // Write one value per lane into its own wave's region, then read its
    // opposite half. The guest needs ordering within its wave without s_barrier.
    words.extend([
        alu(0x1A, 10, k(2), 0),
        alu(
            0x1D,
            14,
            k(if !threads.is_multiple_of(64) { 8 } else { 32 }),
            0,
        ),
        alu(0x1A, 14, k(2), 14),
    ]);
    let counter_bytes = (threads.div_ceil(64) * 4).max(8);
    let [op, args] = ds(0x0D, 0, 10, 4);
    words.extend([op | counter_bytes, args]);
    let [op, args] = ds(0x36, 10, 14, 0);
    words.extend([op | counter_bytes, args]);

    if threads > 64 {
        words.extend([0xBF8A_0000, alu(0x1D, 14, k(4), 11)]); // s_barrier, then other wave's LDS
        words.extend(ds(0x36, 7, 14, 0));
    } else {
        words.push(mov(7, v(5)));
    }
    words.extend([alu(0x1A, 15, k(2), 0), 0xBE85_0380]); // output offset and table address high
    for (slot, reg) in [4, 5, 6, 9, 7, 10, 0].into_iter().enumerate() {
        words.extend([
            0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, // s_load_dwordx4 s[8:11], s[4:5]
            125 << 25 | (slot as u32 * 32),
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0xE070_1000,
            0x8002_0000 | reg << 8 | 15, // buffer_store_dword v<reg>, v15, s[8:11], 0 offen
        ]);
    }
    words.push(0xBF81_0000);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 4,
            slot: 0,
        }],
        used: (0..OUTPUTS as u32)
            .map(|slot| UsedResource {
                kind: ResourceType::Uav,
                root: RootType::Table,
                table_offset_dwords: slot * 8,
                lower_bound: slot,
                stride: 0,
                dwords: 4,
                user_data: 4,
                slot: 0,
            })
            .collect(),
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1 | ((counter_bytes + threads * 4).div_ceil(512).max(2) << 15),
            threads: [threads, 1, 1],
        }),
        code: words.into_iter().flat_map(u32::to_le_bytes).collect(),
    }
}

fn debug_warnings(queue: &ID3D12InfoQueue) -> Vec<String> {
    // SAFETY: All read buffers are aligned and sized by the info queue.
    unsafe {
        (0..queue.GetNumStoredMessages())
            .filter_map(|index| {
                let mut size = 0;
                queue.GetMessage(index, None, &mut size).unwrap();
                let mut words = vec![0usize; size.div_ceil(size_of::<usize>())];
                let message = words.as_mut_ptr().cast::<D3D12_MESSAGE>();
                queue.GetMessage(index, Some(message), &mut size).unwrap();
                let message = &*message;
                (message.Severity.0 <= D3D12_MESSAGE_SEVERITY_WARNING.0).then(|| {
                    std::ffi::CStr::from_ptr(message.pDescription.cast())
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .collect()
    }
}

fn run_guest(
    dims: [u32; 3],
    native: NativeProgram,
    expected: fn(u32, u32) -> [u32; OUTPUTS],
    check_atomic: bool,
) {
    let threads = dims.into_iter().product::<u32>();
    static DEBUG: std::sync::Once = std::sync::Once::new();
    // SAFETY: These opt-in tests run in isolation and enable the debug layer
    // before creating the first device in the test process.
    DEBUG.call_once(|| unsafe {
        let mut debug: Option<ID3D12Debug> = None;
        D3D12GetDebugInterface(&mut debug).expect("D3D12 Graphics Tools");
        let debug = debug.expect("D3D12 debug interface");
        debug.EnableDebugLayer();
        debug
            .cast::<ID3D12Debug1>()
            .unwrap()
            .SetEnableGPUBasedValidation(true);
    });
    let host = Host::new(false).expect("hardware D3D12 adapter");
    // SAFETY: The feature query writes only into the matching D3D12 description.
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    unsafe {
        host.device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS1,
                (&raw mut caps).cast(),
                size_of_val(&caps) as u32,
            )
            .expect("wave capabilities");
    }
    assert_eq!((caps.WaveLaneCountMin, caps.WaveLaneCountMax), (32, 32));
    let queue: ID3D12InfoQueue = host.device.cast().expect("D3D12 info queue");
    // SAFETY: The queue belongs to this test's device.
    unsafe { queue.ClearStoredMessages() };
    // Use the actual host capability and the runtime's per-shader opt-in.
    super::set_host_max_wave(caps.WaveLaneCountMax);
    let mut shader = super::with_wave_pairs(true, || translate_regular(&native)).unwrap();
    validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("DXIL validator")
        .validate(&mut shader.dxil)
        .expect("validate synthetic DXIL");
    let (root, pso) = host.compute_pipeline(&shader).unwrap_or_else(|error| {
        panic!(
            "compute PSO: {error}; D3D12 validation: {:#?}",
            debug_warnings(&queue)
        );
    });
    let emitted = Container::parse(&shader.dxil).unwrap();
    let psv = emitted.part(FourCC::PSV0).unwrap();
    eprintln!(
        "ROUTE: guest={} {dims:?}, physical={}, wave-range={}..{}",
        threads,
        u32_at(psv.data, 40).unwrap(),
        u32_at(psv.data, 20).unwrap(),
        u32_at(psv.data, 24).unwrap()
    );
    let mut views: Vec<_> = (0..OUTPUTS)
        .map(|_| View::RawUav(threads + GUARD as u32))
        .collect();
    if check_atomic {
        views.push(View::RawUav(1));
    }
    let data = vec![0; shader.user_data_dwords().max(1) as usize];
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(data))],
                views: &views,
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .expect("dispatch and readback");
    let warnings = debug_warnings(&queue);
    assert!(warnings.is_empty(), "D3D12 validation: {warnings:#?}");
    assert_eq!(out.len(), views.len());
    if check_atomic {
        assert_eq!(
            out[OUTPUTS],
            [threads],
            "one buffer atomic per present lane"
        );
    }
    let mut failures = 0;
    for (slot, words) in out.iter().take(OUTPUTS).enumerate() {
        assert_eq!(words.len(), threads as usize + GUARD);
        let mut count = 0;
        for thread in 0..threads {
            let want = expected(threads, thread)[slot];
            if words[thread as usize] != want {
                count += 1;
            }
        }
        failures += count;
        eprintln!(
            "CHECK: UAV={slot} mismatches={count}/{threads}; t0={} expected={}; last={} expected={}",
            words[0],
            expected(threads, 0)[slot],
            words[(threads - 1) as usize],
            expected(threads, threads - 1)[slot]
        );
        assert_eq!(&words[threads as usize..], &[0; GUARD], "UAV {slot} guard");
    }
    assert_eq!(failures, 0, "all seven UAVs checked; guards intact");
    let container = Container::parse(&shader.dxil).expect("emitted DXIL container");
    let psv = container
        .part(FourCC::PSV0)
        .expect("pipeline state validation");
    assert_eq!(
        u32_at(psv.data, 40).unwrap(),
        threads.div_ceil(64) * 32,
        "paired metadata: one physical Wave32 per logical Wave64, including a partial wave"
    );
    eprintln!(
        "{threads} guest threads: {OUTPUTS} UAVs match CPU, guards intact, no D3D12 warnings/errors"
    );
}

fn memory_and_exec_guest() -> (NativeProgram, Vec<u32>) {
    let mut native = guest(97);
    let mut counter = native.used[0];
    counter.table_offset_dwords = OUTPUTS as u32 * 8;
    counter.lower_bound = OUTPUTS as u32;
    native.used.push(counter);
    let mut words = vec![
        alu(0x1B, 2, k(63), 0), // logical lane
        alu(0x25, 4, k(11), 2), // nonzero source
        sop1(4, 22, EXEC),
        sop1(4, EXEC, k(0)),
        0x7E00_0400 | 6 << 17 | v(4), // first lane with EXEC empty
        0xD761_0004,                  // writelane v4, 42, lane 1, ignores EXEC
        k(42) | (k(1) << 9),
        0xD760_0007, // readlane s7, v4, 1, ignores EXEC
        v(4) | (k(1) << 9),
        sop1(4, EXEC, 22),
        mov(10, 6),
        mov(11, 7),
        mov(6, k(63)), // sentinel for lanes left out by direct EXEC = 3
        mov(14, k(4)), // source lane one
        sop1(4, EXEC, k(3)),
    ];
    words.extend(ds(0xB3, 6, 14, 4));
    words.extend([
        sop1(4, EXEC, 22),
        alu(0x1D, 14, k(1), 2),
        alu(0x1A, 14, k(2), 14),
    ]);
    words.extend(ds(0xB3, 9, 14, 4)); // neighbor in the same GFX10 half, or zero
    words.extend([
        mov(14, 255),
        1024, // first byte beyond guest LDS, the private counter slot
    ]);
    words.extend(ds(0x0D, 0, 14, 4)); // guest out-of-bounds store is dropped
    words.extend(ds(0x36, 12, 14, 0)); // guest out-of-bounds read yields zero
    words.extend([
        0xBE85_0380,
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        (125 << 25) | (OUTPUTS as u32 * 32),
        0xBF8C_C07F,
        mov(13, k(1)),
        0xE0C8_0000,
        0x8002_0D00, // buffer_atomic_add at byte zero, no return
        0xBF8C_0F70, // wait before the inter-wave rendezvous
        0xBF8A_0000,
        0xE030_0000,
        0x8002_0700, // buffer_load_dword v7 at byte zero
        0xBF8C_0F70,
        alu(0x1A, 15, k(2), 0),
    ]);
    for (slot, reg) in [10, 11, 6, 9, 12, 7, 0].into_iter().enumerate() {
        words.extend([
            0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
            125 << 25 | (slot as u32 * 32),
            0xBF8C_C07F,
            0xE070_1000,
            0x8002_0000 | reg << 8 | 15,
        ]);
    }
    words.push(0xBF81_0000);
    (native, words)
}

fn memory_and_exec_oracle(threads: u32, thread: u32) -> [u32; OUTPUTS] {
    let (wave, lane) = (thread / 64, thread % 64);
    let source = lane ^ 1;
    [
        11,
        42,
        if lane < 2 { 42 } else { 63 },
        if wave * 64 + source < threads {
            if source == 1 { 42 } else { 11 + source }
        } else {
            0
        },
        0,
        threads,
        thread,
    ]
}

#[test]
#[ignore = "requires a wave32 GPU, Graphics Tools, and DXIL validator"]
fn paired_partial_direct_exec_and_memory() {
    let (mut native, words) = memory_and_exec_guest();
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    run_guest([97, 1, 1], native, memory_and_exec_oracle, true);
}

#[test]
fn wave64_partial_group_rejects_unproven_effects() {
    let (native, words) = memory_and_exec_guest();
    let translate = |mut native: NativeProgram, words: Vec<u32>| {
        native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let was = super::lower::TEST_HOST_WAVE.replace(Some(32));
        let pairs = super::TEST_PAIRS.replace(Some(true));
        let result = translate_static(&native);
        super::TEST_PAIRS.set(pairs);
        super::lower::TEST_HOST_WAVE.set(was);
        result.err().map(|e| e.to_string())
    };
    let mut raw = words.clone();
    raw[3] = sop1(4, EXEC, 193); // -1 enables nonexistent logical lanes
    assert!(
        translate(native.clone(), raw).is_some_and(|e| e.contains("bits outside the guest group"))
    );
    let mut lane = words.clone();
    let readlane = lane.iter().position(|&word| word == 0xD760_0007).unwrap();
    lane[readlane + 1] = v(4) | (k(33) << 9); // source is absent in the final 33-lane wave
    assert!(translate(native.clone(), lane).is_some_and(|e| e.contains("not certified")));
    let mut barrier = words.clone();
    let at = barrier.iter().position(|&w| w == 0xBF8A_0000).unwrap();
    barrier.splice(at..at, [cmp(0xD1, k(32), 2), 0xBF88_0001]);
    assert!(translate(native, barrier).is_some_and(|e| e.contains("no proven group rendezvous")));
}

fn translate_static(native: &NativeProgram) -> Result<super::HostShader, crate::ShaderError> {
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes)?;
    let p = crate::ir::build(&native.code, native.wave64, native.vgprs)?;
    super::paired_impl(&c, native, &p, true).map(|(shader, _)| shader)
}

fn translate_regular(native: &NativeProgram) -> Result<super::HostShader, crate::ShaderError> {
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes)?;
    let p = crate::spill::build(native)?;
    let st = crate::structure::structure(&p)?;
    let io = crate::io::raise(&p, native)?;
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st))?;
    super::emit(&c, native, &p, &st, &io, &res)
}

#[test]
fn static_partial_regular_selector_preserves_default_and_supports_explicit_opt_in() {
    let (mut native, words) = memory_and_exec_guest();
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes).unwrap();
    let p = crate::spill::build(&native).unwrap();
    let st = crate::structure::structure(&p).unwrap();
    let io = crate::io::raise(&p, &native).unwrap();
    let res = crate::resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
    super::lower::TEST_HOST_WAVE.set(Some(32));
    super::TEST_PAIRS.set(Some(true));
    let previous = super::emit_legacy(&c, &native, &p, &st, &io, &res);
    assert!(!super::needs_a_wave32_form(&previous));
    let default = translate_regular(&native).unwrap();
    assert_eq!(default.dxil, previous.unwrap().dxil);
    let listed = super::with_wave_pairs(true, || translate_regular(&native)).unwrap();
    assert_eq!(listed.dxil, translate_static(&native).unwrap().dxil);
    assert_ne!(listed.dxil, default.dxil);

    let unproved = guest(96);
    let default = translate_regular(&unproved);
    let listed = super::with_wave_pairs(true, || translate_regular(&unproved));
    match (default, listed) {
        (Ok(a), Ok(b)) => assert_eq!(a.dxil, b.dxil),
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
        _ => panic!("a rejected opt-in replaced the old route"),
    }
    super::TEST_PAIRS.set(None);
    super::lower::TEST_HOST_WAVE.set(None);
}

#[test]
fn static_partial_keeps_implicit_lds_barriers_out_of_divergent_loops() {
    let mut native = guest(96);
    let mut words: Vec<_> = native
        .code
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    let add = ds(0, 0, 11, 13)[0];
    let slot = words
        .iter_mut()
        .find(|w| **w == add)
        .expect("LDS operation in the loop");
    *slot = ds(0x08, 0, 11, 13)[0]; // paired admits ds_max_u32 outside a divergent loop
    native.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let was = super::lower::TEST_HOST_WAVE.replace(Some(32));
    let result = translate_static(&native);
    super::lower::TEST_HOST_WAVE.set(was);
    assert!(
        matches!(result, Err(crate::ShaderError::Unsupported { what, .. })
        if what == "partial paired barrier has no proven group rendezvous")
    );
}

#[test]
#[ignore = "local archive manifest with authorized inputs outside the repository"]
fn static_partial_archive_preserves_the_existing_route() {
    use crate::{io, resource, structure};
    let manifest = std::env::var_os("VICTORY_LOCAL_PARTIAL_AUDIT").expect("local manifest path");
    let items = std::fs::read_to_string(manifest).unwrap();
    super::lower::TEST_HOST_WAVE.set(Some(32));
    super::TEST_PAIRS.set(Some(true));
    let mut partial = 0;
    let mut complete = 0;
    let mut previously_usable = 0;
    let mut preserved = 0;
    let mut newly_usable = 0;
    let mut outside_container_support = 0;
    let mut earlier_stage_failure = 0;
    for (line, file) in items.lines().enumerate() {
        let bytes = std::fs::read(file).unwrap();
        let c = Container::parse(&bytes).unwrap();
        let native = c.part(FourCC::DXIL).and_then(|part| {
            crate::native_programs(part.data)
                .ok()?
                .into_iter()
                .find(|p| p.stage == HwStage::Cs)
        });
        let Some(native) = native else {
            outside_container_support += 1;
            continue;
        };
        let StageIo::Cs(cs) = native.io else {
            unreachable!()
        };
        let threads: u32 = cs.threads.into_iter().product();
        if threads.is_multiple_of(64) {
            complete += 1;
        } else {
            partial += 1;
        }
        let prepared = (|| {
            let p = crate::spill::build(&native)?;
            let st = structure::structure(&p)?;
            let io = io::raise(&p, &native)?;
            let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
            Ok::<_, crate::ShaderError>((p, st, io, res))
        })();
        let Ok((p, st, io, res)) = prepared else {
            earlier_stage_failure += 1;
            continue;
        };
        let previous = super::emit_legacy(&c, &native, &p, &st, &io, &res);
        let next = super::emit_any(&c, &native, &p, &st, &io, &res);
        let usable = |r: &Result<super::HostShader, crate::ShaderError>| {
            r.is_ok() && !super::needs_a_wave32_form(r)
        };
        if usable(&previous) {
            previously_usable += 1;
            let previous = previous.unwrap();
            let next = next.expect("old runnable translation must survive");
            assert!(
                previous.dxil == next.dxil,
                "old runnable case {line} changed"
            );
            preserved += 1;
        } else if usable(&next) {
            newly_usable += 1;
        }
    }
    eprintln!(
        "ARCHIVE partial={partial} complete={complete} outside_container_support={outside_container_support} earlier_stage_failure={earlier_stage_failure} prior={previously_usable} unchanged={preserved} rescued={newly_usable}"
    );
    assert_eq!(
        partial + complete + outside_container_support,
        items.lines().count()
    );
}
