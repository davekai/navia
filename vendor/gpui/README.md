# GPUI

The Linux and Windows app uses GPUI from a pinned [Zed](https://github.com/zed-industries/zed) revision, with a
matching [wgpu](https://github.com/gfx-rs/wgpu). `fetch-gpui.sh` downloads both into `zed/` and `wgpu/`, points
Zed at the local wgpu, and applies the patches in `patches/`. The revisions are at the top of the script.

| Patch | Adds |
| --- | --- |
| `backdrop-blur.patch` | Backdrop blur on Linux (wgpu) |
| `rounded-clips.patch` | Clipping children to rounded corners |
| `kwin-blur-region-origin.patch` | Correct KWin blur placement for windows with client-side shadows |
| `text-fontconfig.patch` | The system's fontconfig hinting and subpixel settings on Linux |
| `mini-player-layer-shell.patch` | Layer-shell margins, to place the mini player on Wayland |
| `windows-directx.patch` | DirectX shaders compiled at runtime, for cross builds, and rounded clips on DirectX |
| `windows-dx-backdrop-blur.patch` | Backdrop blur on DirectX |
| `windows-dx-hit-test.patch`, `windows-dx-nc-hover.patch`, `windows-dx-popup-frame.patch` | Window frame, hit testing and popup fixes on Windows |
| `windows-max-size.patch` | A maximum window size on Windows |
| `transform-scope.patch` | Drawing a subtree scaled and moved, without changing layout or hit testing |
| `wayland-appmenu.patch` | The KDE Plasma global menu address on Wayland |
| `z-clip-fade.patch` | Clips whose right edge fades out |
| `zz-filter-groups.patch` | Content painted offscreen, then blurred, faded and moved as one image |
| `zzz-exact-paint.patch` | Quads and shadows that keep fractional bounds instead of snapping to pixels |
| `zzzz-drop-shadow-cut.patch` | Drop shadows cut out under their element |
| `zzzzz-anonymous-hover.patch` | Hover for elements without an id |

The patches apply in name order. The `z` ones are diffs against the tree with all the earlier patches applied.

GPUI is Apache-2.0. The backdrop blur and rounded clips patches are based on
[Finityno/zed](https://github.com/Finityno/zed) pull requests #62, #74 and #84, also Apache-2.0. The license is
in `patches/LICENSE-APACHE.txt`.
