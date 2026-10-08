# Minecraft 1.21.1 port status

This is active work on Leafish's native Rust client for Minecraft Java 1.21.1,
protocol 767. The intended modded integration target is NeoForge 21.1.236 with
Create 6.0.10. Those targets describe future compatibility work, not current
support. This fork is not a dedicated-server implementation.

The explicit `--local-world` path now supports an early, local Creative session
against a separate **official vanilla Java 1.21.1 server**. It renders real
received terrain using supplied 1.21.1 models/textures, supports ordinary dry
walking, sprinting, jumping, stepping and Creative flight, and sends Creative
block selection, breaking and placement to the server. It skips account loading
and the login screen. This is not complete Minecraft,
survival, NeoForge/Create compatibility, or a Rust server, and no performance
gain has been established.

## Local Creative play

Use your own vanilla 1.21.1 server, bound to `127.0.0.1`, with a **separate new
world**, `online-mode=false`, `gamemode=creative`, `force-gamemode=true`,
`allow-flight=true`, `view-distance=2`, and `simulation-distance=2`. This path
rejects non-loopback servers and does not bypass authentication on a normal
server. For this milestone use peaceful daytime and disable mob spawning;
entity rendering, weather, survival and dimension changes are unfinished.

Generate exact block/item reports with `tools/generate_vanilla_reference.py`
and the schema-two collision/outline/movement report using
[the source-only shape exporter](tools/SHAPE_REFERENCE.md). Generated reports,
assets and game archives are local inputs, not distributed with this fork.

```text
leafish --local-world 127.0.0.1:25587 --profile-dir /path/to/new-profile --client-jar /path/to/client-1.21.1.jar --assets-dir /path/to/assets --asset-index 17 --block-catalog /path/to/blocks.json --item-registry /path/to/registries.json --shape-catalog /path/to/shapes-v2.json
```

Controls: WASD movement, mouse look, Space jump, double-tap Space to toggle
Creative flight, Space/Shift up/down while flying, and Ctrl sprint/faster flight.
Double-tap W also starts ordinary walking sprint.
Left/right click breaks/places blocks, with held-button repetition; 1–9 or the
wheel selects a hotbar slot. E opens the searchable block picker, arrows/Enter
select, Escape releases/resumes controls, and F11 toggles fullscreen.
Closing the client disconnects it; your server launcher is responsible for a
clean world save and shutdown. Saved hotbar contents and selection are retained
when reconnecting; initial blocks are supplied only for a new empty hotbar.
Picker requests use one item, which remains valid for signs and unstackable
shulker boxes; Creative placement does not consume it. This avoids a silently
rejected over-limit stack leaving later selections waiting for confirmation.
The live picker check confirmed signs, shulker boxes and then planks in sequence
through the server before placing the final block.

The hotbar renders supported block-item icons from their installed item models,
textures and inherited GUI transforms. Unsupported item/entity renderers keep
text labels. The picker remains a text list; the complete inventory, arbitrary
item components and full item-rendering parity are unfinished.
Front-facing generated sprites are supported; rotated generated sprites,
conditional overrides, custom color handlers and entity models remain text.
Animated textures use their first declared frame. Four focused icon tests and
a private installed-artwork check covering 12 block items passed; the entity
model for a chest was deliberately rejected rather than replaced with a cube.

The graphical checks used the actual Windows client executable and a separate
vanilla server. A normal-terrain run received 45 chunks and rendered terrain
section meshes. A separate controlled run moved horizontally/upward, broke a
block and placed oak planks through server-authoritative updates and acknowledgments.
Independent decoding of the stopped server's save confirmed the final block,
player position, Creative flight and selected slot. These bounded checks do not
establish full gameplay parity. The updated action check and a subsequent
reconnection also retained the selected oak-plank hotbar slot and saved position.

A new controlled motion run walked over six blocks, stepped onto a half slab,
jumped and landed, then enabled flight through the double-tap gesture. The
reported step height was 0.5 blocks and jump height was 1.252203340253729 blocks.
A separate Java reference invoked the original runtime's input-vector,
ground-acceleration and attribute methods, then evaluated the mapped ordinary
dry-motion equations; its jump apex was 1.2522033402537238. Independently reading
the stopped save confirmed the slab identity, final player position, zero final
motion, Creative flight and selected slot. Intermediate stepping and jumping
are client per-tick observations compared with that reference: the final save
cannot independently prove the intermediate trajectory.

`--capture-frame /path/to/new.png` exports the client's own rendered world after
initial meshing, then exits. Adding `--verify-local-play` enables a bounded
movement/break/place diagnostic that **changes that disposable world**; never
use it on a world you care about. Its JSON report distinguishes unsupported
visual states and does not claim complete parity. Keep captures and reports
private because they include installed artwork and world content.

`--verify-local-motion` is a separate controlled-fixture diagnostic for walking,
half-slab stepping, jumping/landing and flight toggling. It requires a prepared
disposable flat-world fixture and `--capture-frame`; it is not a general-world
self-test and cannot be combined with `--verify-local-play`.
For that fixture, use a grass-block floor at `x=3..10, y=-61, z=-4..16`, clear
the air at `x=3..10, y=-60..-56, z=-4..16`, and place non-waterlogged bottom
stone slabs at `x=3..10, y=-60, z=4`. As `ReformedLocal` joins, issue
`tp ReformedLocal 6.5 -60 0.5 0 0` from the isolated server console before the
diagnostic's initial five-second wait ends. Start with these changes prepared
only in the disposable flat world; the client then walks forward along that path.

Current boundaries: schema-two shapes are exported from the official runtime and
bound to the exact state catalog. They include verified positional plant offsets,
block movement factors, and empty water/lava collision for the ordinary player
context. Remaining dynamic or context-dependent shapes stop affected collision
or targeting explicitly. Schema-one reports remain readable, but cannot supply
the offsets or movement factors required by the updated movement path.

Dry movement uses a standing body and the ordinary, unmodified player attributes.
Support-block selection currently samples beneath the foot center instead of
reproducing the full supporting-block selection at edges. Swimming, crouching,
edge-sneaking, movement effects/equipment attributes, special block callbacks,
entity/world-border collision and complete survival physics are unfinished.
Walking into a water/lava cell reports that swimming is unsupported; empty fluid
collision does not implement fluid travel. Entities/block-entity visuals,
complete item components/inventories/crafting, sound, dimension changes, remote
authentication, NeoForge and mods also remain unfinished. Fluids and biome
tinting have a native rendering path, but partial/waterlogged boundaries, blending, ambient occlusion
and some biome color modifiers are incomplete. The normal legacy server-browser
path still excludes protocol 767; use this explicit local Creative path only.

## Verified components

| Component | Evidence | Boundary |
| --- | --- | --- |
| Configuration packet decoding and terminal state tracking | Unit tests plus an isolated vanilla 1.21.1 exchange: 16 configuration packets, 11 registries, 313 registry entries | Preserves unnamed network NBT and opaque payloads; does not implement mod channels or gameplay |
| Modern section decoding | Source-based unit tests and six synthetic sections serialized by Minecraft 1.21.1's `PalettedContainer.write`; all 24,576 block IDs and 384 biome IDs compared against independent expected values | Establishes section decoding independently of the live test; does not establish rendering |
| Bounded Play session without a renderer | An isolated vanilla 1.21.1 run decoded 45 chunks, 1,080 sections, 4,423,680 block-state values, and 91 light arrays; acknowledged 19 batches, one teleport, and two distinct keepalives in about 30 seconds | Many other Play packets are counted as unimplemented; no renderer or gameplay integration |
| Independent saved-world comparison | All 4,423,680 block-state values and 69,120 biome values from those 45 chunks matched the independently decoded saved world after a clean server shutdown | Compares block and biome identities only; does not independently verify lighting or entity data |
| Signed dimension heights | Validated immutable world bounds, dynamic sections, signed heightmaps, snapshot and dirty-region tests; the 20 legacy chunk fixtures still pass | Local native rendering/collision uses signed sections; the legacy cloud texture is still an 8-bit projection and is omitted from the native path |
| Exact runtime identities and world storage | Caller-supplied state catalog with checked forward/reverse lookup; native chunk store retains raw states, quart biomes, NBT and merged light, with atomic update validation and retention bounds | Separate from the legacy block enum; no unknown state becomes air. Additional material, light-emission and dynamic behavior metadata remain necessary |
| Native runtime adapter | Synthetic TCP tests cover configuration, movement, relative teleport generations, authoritative block changes, action acknowledgments, abilities, FIFO completion and consumer closure | Loopback vanilla reference only; authentication, mods, respawn and reconfiguration remain unsupported. Movement/action encoding is not gameplay parity |
| Actual client adapter-to-store run | The client binary's native check retained 45 chunks / 1,080 sections and matched 4,423,680 block values plus 69,120 biome values to the independently decoded saved world | One movement echo and abilities were observed; this run contained no block updates, separate light updates or block entities. Those paths have synthetic tests, not live parity evidence |
| Local assets and model definitions | Read-only client/resource ZIP mounting, version-correct object-index paths; model tests cover property subsets, exact alternatives, AND/OR, namespaces, parent replacement and texture aliases | Custom model loaders fail explicitly; empty geometry is reported separately, not treated as a successfully rendered entity |
| Modern menu widgets | Buttons and sliders use the installed split sprites and their stretch, tile or nine-slice metadata; five synthetic tests and an optional check of all seven installed sprite states pass | Startup rendering is verified separately; this does not establish world rendering or gameplay |
| Native Creative GUI | Modern store to terrain meshing, exact artwork, biome colors, fluids, modern hotbar and searchable block selection; live normal terrain, confirmed block edits and saved-session reopening | Early local Creative scope above; no full-game or mod parity |
| Block-item hotbar icons | Installed item-model geometry, UVs, GUI transforms and supported flat layers rendered into a bounded icon cache; synthetic tests and a private installed-artwork check | Unsupported/custom/entity models retain text; picker and full inventory rendering remain unfinished |
| Dry walking and Creative flight | Fixed 20 Hz input, jump/gravity/drag, sprinting, momentum handoff and double-tap flight; original-runtime numeric samples and the live motion/save check above | Ordinary standing player only; center-foot support approximation, no swimming/crouching/effects or complete survival physics |
| Collision and targeting | Schema two matches all 26,684 state identities; 26,537 collision and 26,505 outline shapes resolved, with positional offsets, movement factors and candidate-height stepping | 147 collision and 179 outline states remain unresolved; no substitute cubes or air. Fluid collision is not fluid physics |
| Build and public tests | 58 protocol and 103 client tests passed; private reference tests are ignored by default | Compilation and unit tests do not establish complete gameplay; the separate legacy mining failure remains below |

The schema-two generator checks 126 offset-bearing states at 68 positions each
against the original vanilla methods. A separate optional Rust comparison passed
all 272 retained offset/collision/outline samples and all 32 water/lava collision
states. See the [shape reference instructions](tools/SHAPE_REFERENCE.md) for
reproduction and the exact remaining context limits.

The live run received no block entities. Its light-array count demonstrates
decoding and retention, not an independent check of lighting values or rendered
lighting. The roughly 30-second duration includes waiting for keepalive exchanges; it
is not a performance benchmark.

The section fixtures contain artificial numeric identities, including values
outside Leafish's legacy block table. They contain no captured worlds, real
registry definitions, textures, or game archives. See their
[provenance and regeneration instructions](protocol/tests/fixtures/chunks-767/README.md).

The packet and section layouts were checked against locally supplied Minecraft
1.21.1/NeoForge source classes, including `ConfigurationProtocols`,
`ByteBufCodecs`, `NbtIo`, `LevelChunkSection`, `PalettedContainer.Strategy`, and
`SimpleBitStorage`. The reference game archives are not distributed here.

## Pending work

The isolated Play session and independent saved-world comparison have passed.
The client includes the modern adapter and exact native world store. The normal
server browser still uses its legacy path; the explicit local Creative path
connects the modern store to rendering, bounded dry movement and collision,
basic inventory selection and block interaction. Complete behavior remains unfinished. A
rendered model alone does not define a block's rules.

Further work includes applying negotiated registries and dimension properties
throughout the client, complete chunk/light and block-entity behavior, modern
inventories and item components, entities, interaction, rendering, and sound.
The probe counts unsupported Play packets without applying their behavior.
NeoForge negotiation and each required mod's behavior need their own
implementation and tests. Preserving an
unknown ID or custom payload does not make that content functional.

There is currently no verified NeoForge/Create parity, complete 1.21.1 gameplay,
Rust server port, shader/ray-tracing upgrade, or measured performance gain.
Protocol 767 remains outside the normal server-browser list. Local Creative
support is limited to the explicit path and evidence above.

## Build and public tests

Use a current stable Rust toolchain with Cargo and your platform's native linker
and development libraries. The old minimum Rust version in the preserved
upstream README has not been revalidated for this fork. The GUI also needs the
platform dependencies described in the upstream build section. Cross-platform
instructions below are commands to reproduce the work, not a claim that every
platform has been tested.

Run these from the repository root:

```text
cargo test --locked -p leafish_protocol
cargo test --locked -p leafish_blocks catalog::
cargo test --locked -p leafish --bin leafish
cargo build --locked -p leafish_protocol --examples
cargo build --locked --release
```

The protocol tests do not require a Minecraft installation or Java. The build
command builds Leafish; local 1.21.1 play needs the explicit inputs above. Legacy
client behavior and tests remain relevant during the port.

The verified public protocol result is 58 passed and one private-fixture test
ignored; the client result is 103 passed and four private tests ignored. That
optional private configuration test also passed when explicitly
run locally; it is not included in the public count. The separate legacy
block-test failure is documented below; the complete workspace test suite is
not reported as passing.

## Menu rendering verification

Minecraft 1.21.1 replaces the old `gui/widgets` texture sheet with separate GUI
sprites. The client now uses those sprites, including their scaling metadata,
when available, and retains the old atlas path for older resource packs. The
texture atlas resize path also reads the old allocation before resizing it and
initializes added layers, avoiding an undersized readback buffer on shrink.

For an isolated startup check, use your own client archive and asset index:

```text
leafish --profile-dir /path/to/separate-profile --client-jar /path/to/client-1.21.1.jar --assets-dir /path/to/assets --asset-index 17 --capture-frame /path/to/new-startup.png
```

This explicit diagnostic exports the fourth rendered startup frame from the
client's own framebuffer and exits. The PNG path must not already exist; the
readback is limited to 64 MiB. It does not capture other applications. Keep the
image private because it contains installed artwork and may show account names
if an existing profile is used. A fresh test profile avoids that issue.

For the optional installed-widget reference test, set
`LEAFISH_GUI_REFERENCE_ARCHIVE` to your own 1.21.1 client archive, then run:

```text
cargo test --locked -p leafish --bin leafish ui::sprites::tests::installed_widget_art_and_metadata_cover_each_state -- --ignored
```

These menu checks establish artwork and startup rendering only. The menu
background is an image; it is not a rendered world. The separate local Creative
world-rendering checks are described above. The modpack remains unsupported.

## Native client world verification

The actual client binary can exercise its modern adapter and native world store
without opening a window or writing a profile. Use the isolated vanilla server
and exact generated catalog described below:

```text
cargo run --locked -p leafish -- --verify-native-world 127.0.0.1:25586 --block-catalog ../local-reference-1.21.1/generated/reports/blocks.json
```

The bounded run applies chunk, light, unload and authoritative block-change
events in order. It echoes a server-provided player transform and requires
abilities, the player's chunk, completed batches and two keepalives. Completion
uses an ordered checkpoint after the server's batch/bundle is closed; concurrent
network statistics cannot bypass pending world events. The output fingerprints
the actual native store. After a clean server shutdown, the saved-world verifier
below accepts this report as well as the protocol-only probe's report.

This mode rejects non-loopback destinations before connecting and uses no account
credentials. It does not enter the graphical local Creative path.

The local run passed against the separate vanilla reference server, using the
actual client executable rather than the protocol example. It retained 45
chunks, confirmed one teleport, sent one movement echo, received player
abilities, acknowledged 19 batches and answered two keepalives. The native
store's complete block/biome fingerprints matched the saved world after clean
server shutdown. The run took about 30.5 seconds while waiting for keepalives;
it is not a performance measurement. Unsupported packets are listed in the
private report, not silently described as implemented.

## Local assets and isolated profiles

`--client-jar` now mounts an explicitly provided archive read-only and bypasses
the legacy version's downloads and extracted cache. `--assets-dir` together with
`--asset-index` selects the installed objects index. Repeat
`--resource-pack-archive` to layer local ZIP/JAR assets in low-to-high priority
order. An archive's presence does not implement its mod's custom model loader or
gameplay. No game assets are included in this repository.

`--profile-dir` selects separate settings, cache and data directories before the
client initializes logging or settings. Always use a new profile for development
tests; neither the profile nor any test server should point at an existing game
or production world. The normal GUI version gate still excludes protocol 767.

For the optional headless installed-model audit, set `LEAFISH_BLOCK_CATALOG` to
the generated `blocks.json` and `LEAFISH_CLIENT_JAR` to the local client archive:

```text
cargo test --locked -p leafish_blocks catalog::tests::installed_catalog_exact_round_trip -- --ignored
cargo test --locked -p leafish --bin leafish model::definition::tests::installed_catalog_models_resolve_without_graphics -- --ignored --nocapture
```

The model audit uses the production selector/resolver, checks every catalog state
and all selected model alternatives, and verifies referenced face textures exist.
It reports empty models separately. This does not verify rendered pixels, animated
entities, textures' visual correctness, material rules or mod behavior.

The local 1.21.1 audit passed for all **1,060 blocks and 26,684 states**. It
resolved 1,921 reachable model definitions: 1,849 with static geometry and 72
empty models; 18,353 faces referenced 971 PNG textures, all present. Fifty states
selected no multipart applications. Empty models include intentionally invisible
blocks, fluids and blocks requiring entity renderers; none are classified as
working gameplay or replaced with air. The catalog's exact ID/name/property
round-trip also passed for all 26,684 states. These counts cover vanilla assets
only, not the installed mods' custom loaders or artwork.

## Optional local configuration probe

Supply a separate vanilla Minecraft 1.21.1 reference server, bound to loopback
and configured for offline testing. It should use a disposable world and a port
separate from any normal server. Launch and configure that server yourself; the
probe does not create it or accept its license terms.

```text
cargo run --locked -p leafish_protocol --example configuration_probe -- 127.0.0.1:25586
```

The probe rejects non-loopback addresses and online authentication, selects no
cached packs so registry data is sent in full, and stops after configuration.
It does not claim NeoForge channels or run gameplay. Its JSON output describes
that limited result.

## Optional local Play probe

First generate the independent vanilla catalog described below. Its exact
Minecraft 1.21.1 block-state count is **26,684**; the probe requires that count as
an explicit argument. This value is not a substitute for a modded registry.

Use a disposable vanilla 1.21.1 server and world, with these settings in that
separate server's `server.properties`:

```properties
server-ip=127.0.0.1
server-port=25586
online-mode=false
view-distance=2
simulation-distance=2
```

The small distances keep this bounded check within its 64 MiB decoded-data
budget. Start the reference server, then run from the repository root:

```text
cargo run --locked -p leafish_protocol --example play_probe -- 127.0.0.1:25586 26684
```

The probe requires configuration, a joined dimension, a confirmed teleport,
the player's chunk, completed chunk batches, and two distinct keepalive
exchanges before reporting success. It has time, packet, and memory limits and
then closes its connection. It refuses online authentication, mod channels,
respawn, and reconfiguration that it cannot implement. This is a protocol test
without a renderer, not a playable client.

Save the JSON printed to standard output as a local `probe.json`, outside the
public checkout. Keep that report, generated summaries, catalogs, and the
disposable world local; do not commit them. The report lists unimplemented
packet IDs as well as the decoded data and acknowledgments.

### Compare with the saved reference world

Stop your reference server cleanly so it saves its world before running the
independent verifier. Use your private report, world directory, and the
`blocks.json` produced by the catalog generator:

```text
python tools/verify_saved_chunks.py --report ../local-reference-1.21.1/probe.json --world ../local-reference-server/world --blocks ../local-reference-1.21.1/generated/reports/blocks.json
```

This compares decoded block and biome identities against the saved sections.
The isolated reference run passed for 45 chunks: 4,423,680 block-state values
and 69,120 biome values. It does not independently verify lighting, block-entity
data or behavior, entities, inventories, or rendering. Changes made by the
server between the network snapshot and saving can affect a comparison, so use
a controlled disposable reference world and retain the local run details.

## Optional private configuration fixtures

Captured configuration packets are excluded from the public repository. The
captured-frame test is ignored by default. To run it, set
`LEAFISH_CONFIGURATION_FIXTURES` to your private directory containing its
`manifest.json` and the relative body/wire files referenced there, then run:

```text
cargo test --locked -p leafish_protocol --test configuration_reference -- --ignored
```

Explicitly requesting this test without its fixture directory fails with setup
instructions. Keep captured packets, worlds, server configuration, credentials,
and game archives outside the public checkout. Public tests use the included
synthetic fixtures instead.

## Independent vanilla catalog

The configuration registries do not transmit the complete block-state table.
Generate the exact vanilla cardinality and packet-ID reference using your own
official server archive and Java 21. The generator requires a new output
directory and runs the archive's data generator; it does not launch a server.

```text
python tools/generate_vanilla_reference.py --server-jar /path/to/server-1.21.1.jar --output ../local-reference-1.21.1
```

Use `--java /path/to/java` if Java 21 is not on PATH. The generated summary
records archive/report hashes and checks contiguous block-state IDs. The pinned
vanilla reference contains 26,684 states. This value must not be substituted for
a modded server's negotiated state mapping. Keep the generated reports local.

## Known legacy test failure

The existing `leafish_blocks` mining test expects 0.05 seconds for a diamond axe
on vines, while the current implementation returns `MiningTime::Instant`.
The test adapter now handles that enum so the assertion can run; its expected
value remains unchanged. This known failure is separate from the protocol test
suite. No all-tests-pass claim is made for the complete client.

## Contributions and attribution

The starting point is upstream Leafish commit
[`784d76f`](https://github.com/Lea-fish/Leafish/commit/784d76f6836964c746621c506d0051b394d2d31f).
Existing fork work was reviewed before extending the port; see
[FORK_RESEARCH.md](FORK_RESEARCH.md) for exact commits and reuse boundaries.
Leafish's Steven/Stevenarella lineage, upstream credits, and dual MIT/Apache-2.0
licensing remain in place.
