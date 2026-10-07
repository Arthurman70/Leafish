# Minecraft 1.21.1 port status

This is active work on Leafish's native Rust client for Minecraft Java 1.21.1,
protocol 767. The intended modded integration target is NeoForge 21.1.236 with
Create 6.0.10. Those targets describe future compatibility work, not current
support. This fork is not a dedicated-server implementation.

## Verified components

| Component | Evidence | Boundary |
| --- | --- | --- |
| Configuration packet decoding and terminal state tracking | Unit tests plus an isolated vanilla 1.21.1 exchange: 16 configuration packets, 11 registries, 313 registry entries | Preserves unnamed network NBT and opaque payloads; does not implement mod channels or gameplay |
| Modern section decoding | Source-based unit tests and six synthetic sections serialized by Minecraft 1.21.1's `PalettedContainer.write`; all 24,576 block IDs and 384 biome IDs compared against independent expected values | Establishes section decoding independently of the live test; does not establish rendering |
| Bounded Play session without a renderer | An isolated vanilla 1.21.1 run decoded 45 chunks, 1,080 sections, 4,423,680 block-state values, and 91 light arrays; acknowledged 19 batches, one teleport, and two distinct keepalives in about 30 seconds | Many other Play packets are counted as unimplemented; no renderer or gameplay integration |
| Independent saved-world comparison | All 4,423,680 block-state values and 69,120 biome values from those 45 chunks matched the independently decoded saved world after a clean server shutdown | Compares block and biome identities only; does not independently verify lighting or entity data |
| Signed dimension heights | Decoder tests include sections spanning Y=-512 through Y=511 | Does not yet establish that Leafish's world storage, lighting, rendering, or collision supports those heights |
| Build and public protocol tests | Full Windows client build and `leafish.exe --help` passed; 52 protocol tests passed: 50 unit tests and two synthetic-fixture tests | Compilation and help output do not establish 1.21.1 GUI behavior; the private configuration test is ignored by default |

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
Integrating the new session and modern dimensions into Leafish's world,
rendering, and interaction code remains a separate milestone.

Further work includes applying negotiated registries and dimension properties
throughout the client, complete chunk/light and block-entity behavior, modern
inventories and item components, entities, interaction, rendering, and sound.
The probe counts unsupported Play packets without applying their behavior.
NeoForge negotiation and each required mod's behavior need their own
implementation and tests. Preserving an
unknown ID or custom payload does not make that content functional.

There is currently no verified NeoForge/Create parity, 1.21.1 GUI compatibility,
Rust server port, shader/ray-tracing upgrade, or measured performance gain.
Protocol 767 remains outside the normal supported-gameplay list until the
integration is demonstrated. Changes to that claim require reproducible results.

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
cargo build --locked -p leafish_protocol --examples
cargo build --locked --release
```

The protocol tests do not require a Minecraft installation or Java. The build
command builds Leafish; it does not enable 1.21.1 gameplay. Existing legacy
client behavior and tests remain relevant during the port.

The verified public protocol result is 52 passed and one private-fixture test
ignored. That optional private configuration test also passed when explicitly
run locally; it is not included in the public count. The separate legacy
block-test failure is documented below; the complete client's test suite is
not reported as passing.

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
