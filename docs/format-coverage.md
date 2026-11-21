# Xbox Dump Format Coverage

The scan cache for the Xbox v1.0.2.1 dump contains 754,364 recognized entries from 76 archives with scan hits. The counts below come from the whole cache. The decoder audit used up to 65 entries per kind, so the success rates describe the sample, not every file.

| Entries | Count | What 6Stars can show |
| --- | ---: | --- |
| TTPF textures | 357,115 | 42,338 have a BCPack-compressed full-size mip, which 6Stars can't decode. For 13,888 of those (32.8%) a smaller mip is stored as zlib or raw data, so the preview shows that one, labelled "Reduced resolution". The other 28,450 stay blocked. See [BCPack and the lower-mip fallback](#bcpack-and-the-lower-mip-fallback). |
| Models and vehicles | 260,502 | Mesh previews. 65/65 models and 65/65 vehicles produced geometry; this doesn't guarantee full materials, rigging or animation. |
| AWC audio | 71,673 | 65/65 yielded at least one playable stream. |
| NavMesh | 27,629 | 65/65 opened as triangles. |
| GXT | 15,874 | 62/65 yielded readable strings. The rest fell back to hex. |
| Unknown zlib entries | 11,083 | Raw text or hex, not a format-specific preview. Some entries may be chunks rather than complete assets. |
| PSIN | 5,447 | Metadata inspection, not a full editor or renderer. |
| TXD textures | 2,601 | 65/65 yielded textures. |
| RBF0 | 1,262 | 65/65 decoded to XML. |
| BK2 video | 797 | 787 `KB2n`, 10 `KB2j`. Both revisions were tested for playback with the packaged macOS helper; not every clip was played. |
| XML | 294 | 63/65 passed the UTF-8 sample check. |
| Scaleform GFx | 87 | Symbol and metadata inspection, not an interactive Flash preview. |

The shader tab also inspects supported RDNA2 PSO caches separately. The archive scan doesn't inventory every container entry, and it doesn't open loose Xbox files such as DLLs, `gameos.xvd` or `resources.pri`. Files outside the scan or inside encrypted data aren't covered by these totals.

## BCPack and the lower-mip fallback

A `.ttpf` texture stores its whole mip chain, smallest mip first. The store compresses each chunk with zlib, stores it raw, or uses BCPack, the Xbox hardware texture codec, which has no public decoder. When the full-size mip falls in a BCPack chunk, 6Stars decodes the largest smaller mip that sits outside BCPack. The texture preview shows it as "Reduced resolution (WxH of FULLWxFULLH)", and the Model Viewer uses it for materials, so those models show texture colors instead of gray.

Every texture in all 57 stores of the Xbox v1.0.2.1 dump, measured on 2026-11-21:

| Stores | Textures | Full-size mip in BCPack | Recovered from a smaller mip |
| --- | ---: | ---: | ---: |
| `data/ttdb` (46 stores) | 352,461 | 41,390 | 13,784 (33.3%) |
| All 57 (adds `pack/*`) | 357,115 | 42,338 | 13,888 (32.8%) |

Most recovered textures come back at 1/2 to 1/8 of full width (mip 1 to mip 3). None of the 3,368 blocked UI textures could be recovered. 17 more textures sit in chunks flagged as BCPack that are exactly their uncompressed size; the reader treats those as raw data, and they aren't counted above.

The mip layout follows AMD Gfx10 (4 KiB blocks for tile modes 1 to 3, 64 KiB for mode 0, small mips packed into a tail block). On 200 unblocked textures from `ttdb_vehicles.ttpf`, mip 1 matched a 2x2 downsample of mip 0 for all 200. Mip 0 matched the main decoder byte for byte for 197. In the other 3, the main decoder's guess for where mip 0 starts is wrong, so it shows the tail block instead of the image.

To repeat the measurement (`PERCYSTAR_TTPF_ALL=1` covers every store; without it, two stores are checked):

```sh
PERCYSTAR_DUMP=/path/to/dump PERCYSTAR_TTPF_ALL=1 \
  cargo test --release ttpf_lower_mip_fallback -- --ignored --nocapture
```

To repeat the decoder sampling with your own scan cache:

```sh
PERCYSTAR_AUDIT=1 PERCYSTAR_AUDIT_N=65 cargo test --release format_audit -- --ignored --nocapture
```

The audit only reads the BK2 headers. To verify decoded frames, pause and seek against a built Mac bundle, run the silent test with a local clip:

```sh
PERCYSTAR_BINK_APP=dist/6Stars.app PERCYSTAR_BK2=/path/to/sample.bk2 \
  cargo test --release bink_embed -- --ignored --nocapture
```

## Full-size mip placement (2026-11-21)

Where a texture's mip 0 starts isn't fully given by its descriptor. Since v1.6.1, when the original guess and the mip-chain layout disagree (28,137 of 314,841 decodable textures), 6Stars decodes each candidate and keeps the one whose mip 1 is a 2x downscale of its mip 0. Against v1.4.1 this fixes **13,895 textures** that used to show the strip of smaller mips instead of the image (mostly props, maps and vehicles); UI textures are unchanged. Re-run with `PERCYSTAR_DUMP=<dump root> cargo test --release ttpf_mip0_choice_audit -- --ignored --nocapture`.
