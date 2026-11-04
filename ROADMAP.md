# FerroTherm roadmap

Author: Roany Romano. Goal: a free, open-source electronics-cooling solver
that can take over the everyday work done with Cadence Celsius EC Solver,
Simcenter Flotherm/FloEFD or Icepak on defence and industrial enclosures.

**Rule for every phase:** a phase is closed only when what it adds is
complete and usable from the GUI and the command line, covered by tests, and
validated against an analytical solution, a published benchmark or another
solver; the result goes into the *Verification* table of the README. Nothing
that is half done is merged into a release.

## Where we are (November 2026)

Done and validated (see README → Verification):

- Conduction, conjugate CFD (SIMPLEC, Boussinesq), transient, internal
  surface-to-surface radiation, solar load, k(T).
- Turbulence: LVEL, **standard k–ε with scalable wall functions** for
  momentum and heat and buoyancy production, laminar.
- Fans (P–Q curve), vents, inlets, grilles/perforated plates, plate-fin
  heat sinks, monitor points.
- Compact models: 2R and DELPHI multi-node, SD-18 derating check.
- PCB: layered stack, ECAD copper/via maps per tile, **polygon outlines and
  cut-outs** (any board shape, also for blocks in general).
- CAD import: STEP (OpenCascade), STL, IDF, KiCad, Gerber/Excellon,
  IPC-2581, ODB++, Cadence Celsius `.equipment` (chassis, boards in any 90°
  orientation, parts, heat sinks, obstructions, CAD solids, polygon boards,
  Detailed packages split into body and internal parts with the Celsius
  minimum cells, rack models: wall fans and vents, power supplies, chassis
  bays with servers, card bays, drive bays, chip and DIMM sockets).
- **CAD defeaturing editor** (OpenCascade B-rep): holes, bosses, fillets,
  chamfers, small parts, box replacement; GUI and CLI.
- Export: STEP, STL, IDF (with cut-outs), VTK, CSV, Markdown/JSON report.
- Desktop app (Windows/macOS/Linux) and CLI.

## Phase 1 — Celsius import, last gaps

- Verify on real models (needs files from users):
  - multi-axis rotations of boards/obstructions (STEP export of a model that
    uses them);
  - the frame of the internal-part locations of "Detailed" packages (today
    three frames are tried and the import reports which one fitted);
  - numerical comparison of a second model against Celsius temperatures.
- Import cold plates, sensors, fans and vents not mounted on a wall, card
  bays with slots side by side, thermal interface materials (contact
  resistances: needs a contact-resistance model in the core).
- Done when: every object of the two reference models is imported, the
  comparison table covers both, no "not supported" warnings remain on them.

## Phase 2 — Turbulence and wall treatment

- k–ω SST (Menter 1994) with automatic wall treatment (y⁺ insensitive),
  needed for the low-Reynolds flows where k–ε with wall functions
  underestimates heat transfer.
- y⁺/y* reported per wall in the results.
- Validation: turbulent channel (already for k–ε), backward-facing step
  (reattachment length, Driver & Seegmiller 1985), heated block in a channel
  against published Nusselt data, de Vahl Davis cavity at higher Ra.

## Phase 3 — Grid

- Local refinement around boards and components (nested Cartesian
  regions or octree with hanging nodes), keeping the conservative face
  fluxes.
- Grid report: aspect ratios, cells across gaps, warnings for gaps closed by
  the grid.
- Validation: same results as a uniform fine grid at a fraction of the cells.

## Phase 4 — Interoperability

- ECXML (JEDEC JEP181) import and export, the neutral exchange format of the
  electronics-cooling tools.
- FloXML (Flotherm XML) import where documented samples are available.
- Done when round trips and at least one model from each tool import with
  matching geometry.

## Phase 5 — Defeaturing 2

- Pick faces in the 3D view and suppress them (manual override of the
  automatic recognition), saved with the case so the import is repeatable.
- Recognition of holes stored as B-spline surfaces, slots and pockets;
  closing of thin gaps between parts.
- Done when a real chassis STEP from production goes through without manual
  CAD work.

## Phase 6 — Solver performance

- Algebraic multigrid for pressure and energy, better thread scaling,
  benchmark suite with timings in the README.
- GPU solver investigated afterwards.

## Phase 7 — Liquid cooling and multiple fluids

- Second fluid in closed loops (cold plates, liquid manifolds), pumps with
  curves, fluid–solid conjugate heat transfer with different fluids.

## Phase 8 — Scenarios, scripting, optimisation

- Command journal (every GUI action replayable), scripting of parametric
  studies, solution of many scenarios in batch, comparison of results.

## Phase 9 — Results

- Surface maps on parts, heat-path view, report templates for test
  documentation, comparison with measured temperatures.

## Phase 10 — Distribution

- Installers (MSI, notarised macOS app), user manual in Italian and English,
  tutorials with the example models.

## Open questions

- Licences of datasets used for validation must allow redistribution in
  the repository; otherwise the test only runs where the data is available
  (as for the real KiCad and Celsius files today).
- The Celsius `.equipment` format is reconstructed from saved files, not
  from a specification: every new object type needs a real sample.
