# SGD5 metadata and shader interpretation

SGD5 contains compiled shaders plus an effect database describing how the engine may organize, select, and initialize them. This interpretation comes from the supplied two-stream archive; the game's loader has not been examined.

## What the parts provide

| Part | Observed contents | Likely engine role |
| --- | --- | --- |
| Header binding tables | Resource-name hashes, packed binding kinds, indices, group counts, table identities | Reusable binding layouts; effects reference these by hash |
| Shared reflection | Resource descriptions and member layouts reused across shader/effect records | Common logical resource catalog and layout metadata |
| Per-stage shader arrays | Compiled containers, shader hashes, wave-size byte, opaque 144-byte metadata, reflection | Shader variants available to the effect database |
| Per-shader reflection | Resources and typed/packed member records | Logical resource usage and value layouts for that variant |
| Effect shader index arrays | References into each stage array | The variants available to that effect |
| Effect metadata tables | Opaque state/continuation data and counted tables | Technique/pass selection, state, and layout linkage are candidates; exact fields remain unresolved |
| Main effect reflection | Resources, members, auxiliary records, strings, default data | Effect/material parameter descriptions and initialization data |
| Secondary effect reflection | The same table format, often a CBV with auxiliary records | Additional effect/property metadata; exact runtime use is unresolved |
| Effect property list | Inline binding layouts and references to header layouts | Distinct resource layouts, with shared groups and locally stored groups |
| Final effect string pool | Names such as `ClearCube`, `GenerateCube`, `p0`, `p1` | Technique/pass-name candidates, rather than a complete resource-name catalog |

The effect index arrays collectively reference every shader in both supplied streams: 26,503 shaders and 932 effects, with no unreferenced shader indices. This does not establish a one-to-one mapping from pass names to stage-array positions.

Expanding an effect's property list does not produce a complete inventory of its reflection hashes. Member variables can belong to a CBV instead of appearing as individual binding entries. Conversely, shared tables contain broader resource sets than any one effect uses.

Reflection and binding tables describe different relationships. Reflection supplies logical resource/member types, names, packing, auxiliary metadata, and default data. A binding table maps resource hashes into an ordered layout with SRV/UAV/sampler/CBV groups. The grouping and reuse suggest separate cached layouts for passes or variants, but the exact choice of table requires interpreting the remaining effect metadata.

Effects are plausible engine-facing definitions for rendering or compute work: look up an effect, select a technique/pass/variant, choose its stage shaders and state, populate its resource tables and constant data, then submit work. The archive does not establish whether the runtime calls this an effect, material, pipeline family, or another object.

## Confirmed metadata and limits

- Resource records have byte-sized slot/index, auxiliary count, member count, and flags fields. Their slot/index is not confirmed as an HLSL resource register.
- Member records have a byte-sized value type and auxiliary count, array size, packing offset, default-data offset, and auxiliary-data offset. The second section also describes Buffer element metadata, so it is called `members` rather than `constant_members`.
- Record-relative member and auxiliary links are validated against the supplied archive. The second hash is preserved independently; an alternate effect/material parameter identity remains a hypothesis.
- CBV byte sizes and member packing support layout reconstruction. Type `0x0F` remains unresolved, and defaults are initialization candidates rather than compile-time constants.
- `ISG1`/`OSG1` and supported legacy signature chunks supply compiled stage input/output semantics, component types, masks, streams, and signature register indices. These signature registers are not RDNA2 VGPR assignments or texture/buffer binding registers.
- The shader's opaque 144-byte metadata and the compiled `PSV0`/`PRIV`/FSSM resource data may help connect logical resources to descriptor/SGPR accesses; that linkage has not been established.

## Viewer and exports

The SGD5 Shader view retains GFX10.3 disassembly, experimental HLSL lifting, and SPIR-V output. Its Metadata view works independently of FSSM extraction and displays:

- DXBC input/output signature elements, when decoding succeeds.
- FSSM version, declared program range, stage-pointer field, and embedded compiler digest comparison.
- Resource families and exact type codes, metadata slot indices, both hashes, member/auxiliary counts, and CBV sizes.
- Member types, arrays, packing offsets, and raw default-byte previews.
- Shared reflection records with matching shader hashes.
- Referencing effects, effect string pools, and inline/shared binding-table summaries with matching shader hashes and table indices.

The old SGD5 path fabricated HLSL declarations such as `Texture2D<float4> DensityMap : register(t19)` from a resource type and metadata slot. This path has been removed. Container metadata is displayed and exported as labeled comments, preserving its distinction from an established machine binding.

ASM and HLSL text exports include these comments. Metadata can also be exported as a standalone text file. SPIR-V binary generation is unchanged; no unconfirmed resource names or register mappings are introduced into it.

## FSSM program structure

All 26,503 supplied FSSM blobs have magic `FSSM`, version field `0x101`, and a u64 payload size equal to the file length minus `0x10`. Relative offsets use that `0x10` payload base.

The pointer table at file offsets `0x18..0x78` has one populated entry per blob; other entries contain `DEADBEEFDEADBEEF`. The populated entry points to the program descriptor at file offset `0xC0` (relative value `0xB0`). Its first two u64 values are program offset and program size. The program offset is also payload-relative; the size includes compiler no-ops and code-end allocation padding.

The pointer-table field varies with the compiled stage/variant: observed payload-relative fields include `0x18`, `0x20`, `0x28`, `0x30`, `0x48`, and `0x60`. Thus `payload+0x48` is one observed field, not a universal location.

Every supplied program begins with two 12-byte VOP3 `v_nop` instructions. Their ignored fields encode 128 bits: each no-op carries 16 bits in its first word, two data bytes in its source fields, and a 32-bit literal extension. The reconstructed value matches the DXBC `HASH` chunk's 16-byte compiler digest. This compiler hash is distinct from the SGD5 SGA hash and resource/member name hashes.

`BFA00003` is GFX10 `s_inst_prefetch 3`, rather than `s_setprio`. It occurs at program offset `0x18` in 25,873 shaders. In another 630 shaders, `s_swappc_b64 s[4:5], s[4:5]` precedes it, placing prefetch at `0x1C`. Searching for that byte pattern dropped both the no-ops and these real calls. FSSM extraction now follows the declared range and does not depend on this instruction's presence.

Disassembly preserves offsets from the program's beginning and recognizes the no-ops, prefetch, and scalar PC operations. An `s_endpgm` is an instruction, not a container size field: decoding continues after it until code-end padding or the declared range's end. Branch/call structure is still not fully reconstructed by the experimental HLSL/SPIR-V lifters.

The implemented structured FSSM reader supports the observed `0x101` format with one active program. Other versions or multiple active programs are reported as unsupported rather than selected heuristically.

Opcode references: LLVM's [GFX10 VOP1/VOP3 tables](https://github.com/llvm/llvm-project/blob/llvmorg-18.1.6/llvm/lib/Target/AMDGPU/VOP1Instructions.td) and [scalar instruction tables](https://github.com/llvm/llvm-project/blob/llvmorg-18.1.6/llvm/lib/Target/AMDGPU/SOPInstructions.td).

## Improving decompilation later

A useful next step is to connect the compiled resource metadata and effect layout selection to actual descriptor accesses in the ISA. That requires following descriptor table bases, register spaces/ranges, SGPR user data, and bindless indirection rather than equating a name hash or SGD5 index with a machine register.

Once that chain is established, resource types can annotate descriptor loads, member offsets can identify constant-buffer accesses, signatures can describe stage I/O, and effect/variant ownership can organize related shaders. The current viewer exposes the evidence for that work without claiming that the chain is solved.
