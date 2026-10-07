# Synthetic section fixtures for protocol 767

These fixtures contain artificial integer identities. They contain no captured
worlds, vanilla registry definitions, block names, textures, or game archives.
The Rust tests run without Java or a Minecraft installation.

The handwritten [`PaletteReference.java`](../../../../tools/PaletteReference.java)
harness creates two `IdMapper<Object>` registries, with 65,536 block-state
identities and 64 biome identities. It asks a separately supplied Minecraft
1.21.1 `PalettedContainer.write` implementation to serialize them. No Minecraft
encoder implementation is copied into the harness.

The six sections exercise single-value, local-palette and direct representations:

| Section | Block identity base | Distinct block identities | Distinct biome identities | Nonempty count |
| --- | ---: | ---: | ---: | ---: |
| 0 | 50,000 | 1 | 1 | 4,096 |
| 1 | 17,000 | 20 | 3 | 4,096 |
| 2 | 17,000 | 256 | 8 | 4,096 |
| 3 | 17,000 | 257 | 9 | 4,096 |
| 4 | 17,000 | 4,096 | 64 | 4,096 |
| 5 | 0 | 1 | 1 | 0 |

`sections.bin` contains the six section bodies. The two `expected-*-u32le.bin`
files contain independent numeric oracles written directly from the harness's
input loops, with X varying fastest, then Z, then Y. The large identities are
deliberately unrelated to the client's legacy block table.

`parameters.json` records decoder inputs. `provenance.json` records hashes of
the original generated files, the handwritten generator, and the optional named
reference archive used to produce this fixture set. That archive is not included.
The published generator hash uses LF line endings; the captured generator hash
retains the original capture's source-byte provenance.

## Run the public tests

From the repository root:

```text
cargo test --locked -p leafish_protocol --test chunk_reference
```

## Regenerate independently

Use JDK 21. Supply your own named Minecraft 1.21.1 reference archive and its
runtime dependencies, including Netty, through `MINECRAFT_REFERENCE_CLASSPATH`.
No archive is downloaded by these commands. Generate into the ignored `target`
directory first, then compare the output hashes against `provenance.json`.

On a POSIX shell:

```sh
mkdir -p target/palette-reference
javac -cp "$MINECRAFT_REFERENCE_CLASSPATH" -d target/palette-reference tools/PaletteReference.java
java -cp "target/palette-reference:$MINECRAFT_REFERENCE_CLASSPATH" PaletteReference target/palette-reference/generated
```

On PowerShell:

```powershell
New-Item -ItemType Directory -Force target/palette-reference | Out-Null
javac -cp $env:MINECRAFT_REFERENCE_CLASSPATH -d target/palette-reference tools/PaletteReference.java
java -cp "target/palette-reference;$env:MINECRAFT_REFERENCE_CLASSPATH" PaletteReference target/palette-reference/generated
```

The captured configuration-packet test is separate from these synthetic fixtures.
Its data is excluded from Git, and the test is ignored by default. To opt in,
set `LEAFISH_CONFIGURATION_FIXTURES` to your private directory containing its
`manifest.json` and relative body/wire files, then run:

```text
cargo test --locked -p leafish_protocol --test configuration_reference -- --ignored
```

Explicitly running that test without the environment variable fails with setup
instructions; it does not silently claim conformance.
