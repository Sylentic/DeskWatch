# Icons

The DeskWatch icon: the status ring (green done, orange running) on the Graphite background. The bridge builds these
files in and serves them without a login, so the browser tab shows the icon on every page, the login page included.

| File | Served on | Use |
|---|---|---|
| `favicon.svg` | `/favicon.svg` | The tab icon, sharp at every size. The source of the other files. |
| `favicon-32.png` | `/favicon-32.png` | Fallback for browsers without SVG tab icons. |
| `apple-touch-icon.png` | `/apple-touch-icon.png` | 180x180, square without rounded corners (the phone rounds them), for "Add to Home Screen". |

The logo for the README and GitHub (512 px PNG, social preview) is in `docs/logo/`. After a change to `favicon.svg`,
render the PNGs again from it at the sizes above.
