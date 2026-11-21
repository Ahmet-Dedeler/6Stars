# Format Coverage

What 6Stars can show for each kind of entry its scanner recognizes. Coverage was measured by running the opt-in decoder audit (below) on a large scan of user-supplied, unencrypted archives: the audit samples up to 65 entries per kind, so the notes describe the sample, not every file. This repository contains no game files, and none are needed to build or test it.

| Entries | What 6Stars can show |
| --- | --- |
| TTPF textures | Preview and PNG/DDS export. Some textures keep their full-size mip in BCPack, which 6Stars can't decode; for about a third of those a smaller mip is stored as zlib or raw data, and the preview shows that one, labelled "Reduced resolution". See [BCPack and the lower-mip fallback](#bcpack-and-the-lower-mip-fallback). |
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

A `.ttpf` texture stores its whole mip chain, smallest mip first. The store compresses each chunk with zlib, stores it raw, or uses BCPack, a hardware texture codec with no public decoder. When the full-size mip falls in a BCPack chunk, 6Stars decodes the largest smaller mip that sits outside BCPack. The texture preview shows it as "Reduced resolution (WxH of FULLWxFULLH)", and the Model Viewer uses it for materials, so those models show texture colors instead of gray.

In the measured scan, about a third of BCPack-blocked textures could be recovered this way, most at 1/2 to 1/8 of full width (mip 1 to mip 3). Blocked UI textures couldn't be recovered. Chunks flagged as BCPack that are exactly their uncompressed size are read as raw data.

The mip layout follows AMD Gfx10 (4 KiB blocks for tile modes 1 to 3, 64 KiB for mode 0, small mips packed into a tail block). On a sample of unblocked textures, mip 1 matched a 2x2 downsample of mip 0 every time, and mip 0 matched the main decoder byte for byte in almost every case. In the rest, the main decoder's guess for where mip 0 starts is wrong, so it shows the tail block instead of the image.

## Re-running the checks on your own files

All of these are opt-in and read only files you point them at.

```sh
# lower-mip fallback across the .ttpf stores in a folder
PERCYSTAR_DUMP=/path/to/your/archives PERCYSTAR_TTPF_ALL=1 \
  cargo test --release ttpf_lower_mip_fallback -- --ignored --nocapture

# decoder sampling over your 6Stars scan cache
PERCYSTAR_AUDIT=1 PERCYSTAR_AUDIT_N=65 cargo test --release format_audit -- --ignored --nocapture

# silent BK2 play / pause / seek against a built Mac bundle
PERCYSTAR_BINK_APP=dist/6Stars.app PERCYSTAR_BK2=/path/to/sample.bk2 \
  cargo test --release bink_embed -- --ignored --nocapture
```

## Full-size mip placement

Where a texture's mip 0 starts isn't fully given by its descriptor. Since v1.6.1, when the original guess and the mip-chain layout disagree, 6Stars decodes each candidate and keeps the one whose mip 1 is a 2x downscale of its mip 0. In the measured scan this fixed thousands of textures (mostly props, maps and vehicles) that used to show the strip of smaller mips instead of the image; UI textures are unchanged. Re-run with `PERCYSTAR_DUMP=/path/to/your/archives cargo test --release ttpf_mip0_choice_audit -- --ignored --nocapture`.
