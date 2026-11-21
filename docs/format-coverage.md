# Xbox Dump Format Coverage

The scan cache for the Xbox v1.0.2.1 dump contains 754,364 recognized entries from 76 archives with scan hits. The counts below come from the whole cache. The decoder audit used up to 65 entries per kind, so the success rates describe the sample, not every file.

| Entries | Count | What PercyStar can show |
| --- | ---: | --- |
| TTPF textures | 357,115 | 53/65 decoded. The other 12 hit BCPack chunks. The app knows the texture format but can't read the pixel data from those chunks. |
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

To repeat the decoder sampling with your own scan cache:

```sh
PERCYSTAR_AUDIT=1 PERCYSTAR_AUDIT_N=65 cargo test --release format_audit -- --ignored --nocapture
```

The audit only reads the BK2 headers. To verify decoded frames, pause and seek against a built Mac bundle, run the silent test with a local clip:

```sh
PERCYSTAR_BINK_APP=dist/PercyStar.app PERCYSTAR_BK2=/path/to/sample.bk2 \
  cargo test --release bink_embed -- --ignored --nocapture
```
