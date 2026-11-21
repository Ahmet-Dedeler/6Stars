# Format Coverage

What Percy6Stars can show for each kind of entry its scanner recognizes. Coverage was measured by running the opt-in decoder audit (below) on a large scan of user-supplied, unencrypted archives: the audit samples up to 65 entries per kind, so the notes describe the sample, not every file. This repository contains no game files, and none are needed to build or test it.

| Entries | What Percy6Stars can show |
| --- | --- |
| TTPF textures | Preview and PNG/DDS export, including BCPack BC1. Textures whose full-size mip uses another BCPack variant (BC3 to BC7) show the largest smaller mip that decodes, labelled e.g. "Top mips are BCPack BC7; showing mip 2". See [BCPack and the lower-mip fallback](#bcpack-and-the-lower-mip-fallback). |
| Models and vehicles | Mesh previews. Every sampled model and vehicle produced geometry; this doesn't guarantee full materials, rigging or animation. |
| AWC audio | Every sampled bank yielded at least one playable stream. |
| NavMesh | Every sampled tile opened as triangles. |
| GXT | Most sampled tables yielded readable strings; the rest fall back to hex. |
| Unknown zlib entries | Raw text or hex, not a format-specific preview. Some entries may be chunks rather than complete files. |
| PSIN | Metadata inspection, not a full editor or renderer. |
| TXD textures | Every sampled dictionary yielded textures. |
| RBF0 | Every sampled resource decoded to XML. |
| BK2 video | `KB2n` and `KB2j` revisions play in the preview panel with the packaged decoder. |
| XML | Nearly every sample passed the UTF-8 check. |
| Scaleform GFx | Symbol and metadata inspection, not an interactive Flash preview. |

The shader tab inspects supported RDNA2 PSO caches separately. The archive scan doesn't inventory every container entry, and it skips files that aren't archives. Encrypted data is never read or decrypted, so it isn't covered.

## BCPack and the lower-mip fallback

A `.ttpf` texture stores its whole mip chain, smallest mip first. The store compresses each chunk with zlib, stores it raw, or uses BCPack, the Xbox Series hardware texture codec (a rANS entropy layer over BCn blocks). The chunk table says which BCn variant a BCPack chunk holds.

**BC1** chunks are decoded with [bcpack-rs](https://github.com/Ahmet-Dedeler/bcpack-rs). Its entropy layer uses the exact model tables from Microsoft's [silicon-rans](https://github.com/microsoft/silicon-rans) Verilog; its block layer uses two small tables fitted from data, so a few blocks can come out with slightly off colours. A chunk the decoder rejects is treated like any other undecodable BCPack chunk. BC1 is by far the most common variant: on six measured stores, 2,517 of the 2,616 textures whose full-size mip touches BCPack were BC1, and all of them now decode at full size (before, 1,594 showed a smaller mip and 923 showed nothing).

For the other variants (BC3 to BC7), Percy6Stars decodes the largest smaller mip whose chunks are all decodable (raw, zlib or BCPack BC1). The texture preview says which variant blocked it and which mip it shows, e.g. "Top mips are BCPack BC7; showing mip 2 (512x512 of 2048x2048)", and the Model Viewer uses that mip for materials, so those models show texture colors instead of gray. On the same stores, 91 of the remaining 99 were recovered this way. Chunks flagged as BCPack that are exactly their uncompressed size are read as raw data.

The mip layout follows AMD Gfx10 (4 KiB blocks for tile modes 1 to 3, 64 KiB for mode 0, small mips packed into a tail block). On a sample of unblocked textures, mip 1 matched a 2x2 downsample of mip 0 every time, and mip 0 matched the main decoder byte for byte in almost every case. In the rest, the main decoder's guess for where mip 0 starts is wrong, so it shows the tail block instead of the image.

## Re-running the checks on your own files

All of these are opt-in and read only files you point them at.

```sh
# lower-mip fallback across the .ttpf stores in a folder
PERCYSTAR_DUMP=/path/to/your/archives PERCYSTAR_TTPF_ALL=1 \
  cargo test --release ttpf_lower_mip_fallback -- --ignored --nocapture

# decoder sampling over your Percy6Stars scan cache
PERCYSTAR_AUDIT=1 PERCYSTAR_AUDIT_N=65 cargo test --release format_audit -- --ignored --nocapture

# silent BK2 play / pause / seek against a built Mac bundle
PERCYSTAR_BINK_APP=dist/Percy6Stars.app PERCYSTAR_BK2=/path/to/sample.bk2 \
  cargo test --release bink_embed -- --ignored --nocapture
```

## Full-size mip placement

Where a texture's mip 0 starts isn't fully given by its descriptor. Since v1.6.1, when the original guess and the mip-chain layout disagree, Percy6Stars decodes each candidate and keeps the one whose mip 1 is a 2x downscale of its mip 0. In the measured scan this fixed thousands of textures (mostly props, maps and vehicles) that used to show the strip of smaller mips instead of the image; UI textures are unchanged. Re-run with `PERCYSTAR_DUMP=/path/to/your/archives cargo test --release ttpf_mip0_choice_audit -- --ignored --nocapture`.

Since v1.7.0 there is one more candidate: the tile patterns and mip placement from [paryzius/ttpf-viewer](https://github.com/paryzius/ttpf-viewer) (`src/ttpf_tile.rs`, MIT). It wins when its mip pair matches, and when there is no mip pair to compare (single-tile icons, chains whose mip 1 shares a tile) it wins if its image is clearly less blocky, if only the untiling differs for an uncompressed texture, or if the old read is all black. Compared pixel by pixel against ttpf-viewer's own export, matching textures in a graphics store went from 166 to 877 of 901, and a character store went from 89 real differences to 3 (BC5 normal maps still differ only because Percy6Stars rebuilds the blue channel). Where ttpf-viewer reads padding (some BC7 normal maps), the existing layouts still win. `ttpf_pattern_guess_wins` covers both directions.
