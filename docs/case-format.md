# FerroTherm case file reference

Case files are TOML. **Units:** lengths mm, temperatures °C, power W, heat
transfer coefficient W/(m² K), time s. Unknown keys are rejected, so typos
are reported instead of being silently ignored.

Overlapping geometry is resolved by order: later `[[blocks]]` overwrite
earlier ones, and `[[components]]` overwrite blocks. Cells are assigned by
their centre.

## `[meta]` (optional)

| Key | Type | Notes |
|---|---|---|
| `title`, `author`, `description` | string | Copied into the report |

## `[domain]`

| Key | Type | Notes |
|---|---|---|
| `min`, `max` | [x, y, z] | Bounding box of the model |
| `material` | id | Fill material, default `air-still` |

## `[grid]` (optional)

| Key | Default | Notes |
|---|---|---|
| `max_cell_mm` | 2.0 | Scalar or `[dx, dy, dz]` |
| `min_cells_per_interval` | 1 | Minimum cells between two consecutive geometry planes |

The grid always contains every block, component and boundary-region plane.
PCB blocks get at least 4 cells through their thickness.

## `[environment]` (optional)

| Key | Notes |
|---|---|
| `preset` | Id from `ferrotherm environments`, e.g. `mil-std-810h-a1-hot-dry` |
| `use` | `ambient_max` (default), `ambient_min`, `induced_max`, `induced_min` |

Sets the default ambient for boundaries without `t_ambient_c`, and the
initial temperature.

## `[[blocks]]`

| Key | Notes |
|---|---|
| `name` | Required, used in the report |
| `material` | Library id — **or** — |
| `pcb` | Inline table, see below |
| `min`, `max` | Box corners |
| `power_w` | Uniform heat load of the block, default 0 |

### PCB stack (`pcb = { ... }`)

| Key | Default | Notes |
|---|---|---|
| `thickness_mm` | — | Total board thickness |
| `layers`, `copper_oz`, `coverage` | — | Uniform shorthand… |
| `copper_layers` | — | …or explicit `[{ oz = 2, coverage = 0.9 }, ...]` |
| `via_fraction` | 0 | Copper area fraction of thermal vias |
| `dielectric` | `fr4` | Material id |
| `copper` | `cu-c11000` | Material id |
| `normal` | `z` | Axis through the board |

## `[[components]]` — 2R compact models

| Key | Notes |
|---|---|
| `name`, `package` | Package id from `ferrotherm packages` |
| `position` | Minimum corner of the body |
| `size` | `[L, W, H]`; default = package nominal size |
| `mount_axis` | Board normal, default `z`; H is measured along it |
| `power_w` | Dissipation at the junction |
| `r_jc`, `r_jb` | K/W, override the package values; at least one required |
| `case_face` | `top` or `bottom`: side connected through `r_jc` (the other side uses `r_jb`) |
| `derating` | Id from `ferrotherm deratings` |
| `tj_max_c` | Explicit Tj limit, overrides `derating` |

The body is thermally inert; heat flows junction → face → neighbouring
cells, distributed by area. Side faces are adiabatic (standard 2R).

## `[[boundaries]]`

Faces of the domain without a boundary are adiabatic. If several
boundaries cover the same spot, the last one wins (an `adiabatic` region can
cut a hole in a convection face).

| Key | Notes |
|---|---|
| `name` | Optional |
| `face` | `xmin`, `xmax`, `ymin`, `ymax`, `zmin`, `zmax` |
| `region` | `{ min = [u0, v0], max = [u1, v1] }` in the face's two in-plane coordinates (x,y for z-faces; x,z for y-faces; y,z for x-faces) |
| `type` | `adiabatic`, `fixed`, `convection`, `radiation` |
| `t_c` | Fixed temperature |
| `h`, `t_ambient_c` | Convection |
| `emissivity` or `finish` | Adds radiation to `convection`; required for `radiation` |
| `t_radiation_c` | Radiation sink, default = ambient |

## `[solver]`

| Key | Default | Notes |
|---|---|---|
| `mode` | `steady` | or `transient` |
| `tolerance` | 1e-9 | Relative residual of the linear solver |
| `max_iterations` | 20000 | PCG iterations per solve |
| `initial_c` | ambient (or 25) | Initial temperature |
| `nonlinear_tolerance_k` | 1e-3 | Radiation iteration threshold |
| `max_nonlinear_iterations` | 100 | |

### `[solver.transient]`

| Key | Notes |
|---|---|
| `dt_s`, `end_s` | Time step and end time (implicit Euler) |
| `output_every_s` | Write a field every N s (default: start and end only) |

## Extra libraries

`libraries = ["my_materials.json"]` at the top level (paths relative to the
case file), or `--library file.json` on the command line. A user file has the
same shape as the built-in ones and may contain any of `materials`,
`finishes`, `packages`, `environments`, `derating`. An entry with an existing
`id` replaces the built-in one.

```json
{ "materials": [
  { "id": "my-gap-pad", "name": "Gap pad X, 5 W/mK", "k": 5.0,
    "density": 3100, "cp": 1000, "source": "Supplier datasheet rev C" }
]}
```
