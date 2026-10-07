# Existing Leafish fork review

Public repository research was performed on 2026-10-07 before further 1.21.1
implementation. The question was whether an existing Leafish branch already
provided Minecraft Java 1.21.1/protocol 767 support that this fork could reuse.

## Scope and result

The review enumerated **62 public Leafish forks including descendants**, then
examined **375 visible branch references** across that network, Leafish upstream,
and Stevenarella upstream. Shared references reduced to **111 distinct branch
heads**. At each available head, the review inspected supported-protocol
selectors, version dispatch, shared version handling, and relevant source
indicators. The most advanced candidates received a deeper source and commit
history review. README claims were not treated as executable compatibility.

The starting inventory was GitHub's paginated
[Leafish forks API](https://api.github.com/repos/Lea-fish/Leafish/forks?per_page=100&sort=newest),
with descendant fork endpoints and branch listings checked separately. The
branch endpoint for `vyamkovyi/Leafish` returned HTTP 404 and could not be
inspected. Counts describe this dated review, not a permanent repository total.

**No 1.21.1/protocol 767 implementation was verified in the branches screened.**
The strongest Leafish candidates contained partial 1.20.4/protocol 765 work.
This is not an exhaustive claim about all public projects, deleted or private
forks, unpushed work, or code outside the inspected paths. These candidates were
reviewed as source; this review did not build or execute their clients.

## Candidates with useful work

| Repository and inspected revision | Code evidence | Assessment |
| --- | --- | --- |
| [pannous/Leafish `887bd24`](https://github.com/pannous/Leafish/commit/887bd24dbfcee756228d86a1cf1e8c977f371cc1) | Adds protocol 765 dispatch, configuration handling, login/JoinGame packets, and a modern section loader; 13 commits ahead of the chosen upstream base | Useful integration reference, with correctness gaps below; not a 767 port |
| [kazini/Leafish `4240ca5`](https://github.com/kazini/Leafish/commit/4240ca53fea502876a8a69ab8ba0d641650a95a6) | Extends the same 765 work; 26 commits ahead of the chosen base | Its [stability change](https://github.com/kazini/Leafish/commit/80b964db953743c8b2f6bd03b619981139c2c7e2) explicitly restores the default to 1.16.5 while modern protocol work remains experimental |
| [Uk-Cat/Rustcraft `f17e8cb`](https://github.com/Uk-Cat/Rustcraft/blob/f17e8cba68881d3352cf752ac5a1a05e2cfb2a1d/protocol/src/protocol/mod.rs#L54) | Supported-protocol array still tops out at 754 | Recent activity did not establish modern protocol support |
| [Stevenarella master `815ac88`](https://github.com/iceiix/stevenarella/blob/815ac883389a871a888ea4436ad1af192cfeca7b/protocol/src/protocol/mod.rs) and [1.19 branch `b43395d`](https://github.com/iceiix/stevenarella/blob/b43395d5112c641bc5615ee9af3855776d2e4e8e/protocol/src/protocol/mod.rs) | Selectors reach 758 on master and 759 on the 1.19 branch | Earlier protocol work in Leafish's lineage; no verified 767 implementation in the upstream branches reviewed |

## Reuse boundaries

The pannous fork provides concrete examples of where login acknowledgment,
configuration handling, and modern packet structures fit into Leafish. Its
[configuration connection helper](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/src/server/mod.rs#L159)
and [765 packet table](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/protocol/src/protocol/versions/v1_20_4.rs)
are references to review when wiring a tested 767 implementation.

Several behaviors should not be imported as compatibility fixes:

- The [block mapping](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/blocks/src/versions/mod.rs#L29) maps the 1.20 version category through the 1.19 block table.
- The [section loader](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/src/world/mod.rs#L1403) assumes 24 sections, retains only Y=0..255, and skips biome data.
- The [configuration helper](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/src/server/mod.rs#L182) ignores registry, feature, and tag packets.
- The [packet reader](https://github.com/pannous/Leafish/blob/887bd24dbfcee756228d86a1cf1e8c977f371cc1/protocol/src/protocol/mod.rs#L1365) skips parse failures, unmapped packets, and packets with unread bytes. Continuing after those failures does not demonstrate protocol correctness.

These are material limits for modern dimension heights and negotiated modded
registries. This fork keeps explicit decoding errors, preserves unknown numeric
IDs and payloads, and validates layouts against independent reference data.
Any reused code should retain attribution and receive tests for the actual 767
wire format. The audit itself does not claim that any candidate code has been
merged, that a client is playable, or that NeoForge/Create is implemented.
