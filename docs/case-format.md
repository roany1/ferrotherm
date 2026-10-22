# FerroTherm case file reference

Case files are TOML. **Units:** lengths mm, temperatures °C, power W, heat
transfer coefficient W/(m² K), time s, volume flow m³/s, pressure Pa. Unknown keys are rejected, so typos
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
| `material` | id | Fill material, default `air-still`. With `[flow]` it must be a fluid with a viscosity (`air`, `water`) |

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
| `emissivity` or `finish` | Surface emissivity for internal radiation (`[radiation]`) |

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

## `[[components]]` — 2R and DELPHI compact models

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

| `network` | Multi-node (DELPHI) model instead of 2R, see below |

The body is thermally inert; heat flows junction → face → neighbouring
cells, distributed by area. Side faces are adiabatic (standard 2R).

### DELPHI network (`[components.network]`)

| Key | Notes |
|---|---|
| `junction` | Node that dissipates the power (default `junction`) |
| `nodes` | `[{ name = "top_inner", surface = "top_inner" }, ...]`; `surface` is one of `top`, `bottom`, `top_inner`, `top_outer`, `bottom_inner`, `bottom_outer`, `sides`, `leads`; omit it for an internal node |
| `links` | `[{ a = "junction", b = "top_inner", r = 4.2 }, ...]`, K/W from the part supplier |
| `inner_size_mm` | `[L, W]` of the inner top/bottom regions (default half the body) |
| `lead_ring_mm` | Width of the ring of board cells around the footprint for `leads` (default 1) |

## `[[boundaries]]`

Faces of the domain without a boundary are adiabatic. If several
boundaries cover the same spot, the last one wins (an `adiabatic` region can
cut a hole in a convection face).

| Key | Notes |
|---|---|
| `name` | Optional |
| `face` | `xmin`, `xmax`, `ymin`, `ymax`, `zmin`, `zmax` |
| `region` | `{ min = [u0, v0], max = [u1, v1] }` in the face's two in-plane coordinates (x,y for z-faces; x,z for y-faces; y,z for x-faces) |
| `type` | `adiabatic`, `fixed`, `convection`, `radiation`, `opening`, `inlet`, `symmetry` |
| `t_c` | Fixed temperature |
| `h`, `t_ambient_c` | Convection; for `opening`/`inlet`, `t_ambient_c` is the temperature of the air coming in |
| `emissivity` or `finish` | Adds radiation to `convection`; required for `radiation` |
| `t_radiation_c` | Radiation sink, default = ambient |
| `heat_flux_w_m2` | Imposed absorbed flux on a `fixed`/`convection`/`radiation` face |
| `solar_w_m2`, `absorptivity` | Solar load (absorbed fraction = `absorptivity`, default 1) |
| `velocity_m_s` or `flow_m3_s` | `inlet`: flow into the domain |
| `loss` | `opening`: grille on the vent, `{ zeta, free_area, hole_diameter_mm }` (see grilles) |

`opening` is a vent at ambient static pressure: air leaves or enters as the
flow requires. `symmetry` closes the face to flow and heat without friction.

## `[flow]` — air flow (CFD)

Present and `enabled = true` (default): the fill material flows. Steady or
transient (follows `[solver] mode`).

| Key | Default | Notes |
|---|---|---|
| `turbulence` | `lvel` | `lvel` (algebraic, wall distance + Spalding law) or `laminar` |
| `scheme` | `power_law` | or `upwind` |
| `gravity` | `[0, 0, -9.81]` | m/s²; z up |
| `reference_temperature_c` | ambient | Boussinesq reference |
| `max_iterations` | 500 | Outer SIMPLEC iterations (steady) |
| `tolerance` | 1e-3 | Normalised mass, momentum and energy residuals |
| `relax_velocity`, `relax_pressure`, `relax_temperature` | 0.6, 0.8, 1.0 | Under-relaxation |
| `iterations_per_step` | 20 | Transient: outer iterations per time step |

A steady run also stops when every junction, monitor point and the hottest
cell vary less than max(0.05 K, 0.2 % of their rise) over 50 iterations
with a low continuity residual (reported as "temperatures settled").

## `[[fans]]`

| Key | Notes |
|---|---|
| `name` | |
| `center` | Centre of the fan face, mm |
| `direction` | `+x`, `-x`, `+y`, `-y`, `+z`, `-z` |
| `diameter`, `hub_diameter` | Round fan (hub carries no flow) — **or** — |
| `size` | `[a, b]` rectangular face in the two in-plane axes |
| `curve` | `[[Q m³/s, Δp Pa], ...]` with increasing Q (datasheet curve) — **or** — |
| `flow_m3_s` | Fixed volume flow |

A fan on the domain boundary draws from (or blows to) the ambient; inside
the domain it is a pressure-jump plane. 1 CFM = 4.7195e-4 m³/s.

## `[[resistances]]` — grilles, perforated plates, filters

| Key | Notes |
|---|---|
| `name`, `center` | |
| `axis` | Normal of the plane: `x`, `y`, `z` |
| `size` | `[a, b]` in the two in-plane axes (increasing axis order) |
| `loss` | `{ zeta = 4.0, free_area = 0.5, hole_diameter_mm = 3 }` |

Δp = ½ρζu² on the approach velocity u, plus the laminar thin-plate term
30/(f²·Re) when `free_area` and `hole_diameter_mm` are given (Idelchik, as
summarised in Electronics Cooling, May 1998). ζ comes from Idelchik, the
grille datasheet or a test.

## `[[heatsinks]]` — plate-fin heat sinks

| Key | Notes |
|---|---|
| `name`, `material` | Material default `al-6061-t6` |
| `position` | Minimum corner of the base |
| `base` | `[length along the fins, width across the fins, thickness]` |
| `normal` | Base normal (fins grow towards +normal), default `z` |
| `fin_axis` | Direction the fins run along, in the base plane |
| `fin_count`, `fin_thickness`, `fin_height` | |
| `emissivity` | Optional |

Expanded into `<name>/base` and `<name>/finN` blocks.

## `[[monitors]]`

`{ name, position }`: temperature and speed recorded every iteration and
used by the convergence check.

## `[radiation]` — internal surface-to-surface radiation

| Key | Default | Notes |
|---|---|---|
| `enabled` | true | |
| `patch_size_mm` | 10 | Surfaces are grouped into patches of about this size |
| `rays_per_patch` | 256 | Monte Carlo rays for the view factors |
| `default_emissivity` | 0.9 | For surfaces without their own value |

The fill is transparent; openings radiate to the ambient. Material
conductivity may depend on temperature (`k_table` in a library material).

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

## `[[materials]]` — materials inside the case

Same fields as a library material (`id`, `name`, `k`, `density`, `cp`,
optional `viscosity`, `beta`, `k_table`). They extend or replace library
entries with the same id; models imported from other tools carry their
materials this way.

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
