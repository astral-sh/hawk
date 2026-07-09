# Changelog

<!-- prettier-ignore-start -->

## 0.1.7

Released on 2026-07-09.

### Other changes

- Scope doctest analysis by package ([#114](https://github.com/astral-sh/hawk/pull/114))

## 0.1.6

Released on 2026-07-09.

### Other changes

- Remove GPT-5.5 from README ([#111](https://github.com/astral-sh/hawk/pull/111))
- Upgrade to Rust 1.97.0 ([#112](https://github.com/astral-sh/hawk/pull/112))

## 0.1.5

Released on 2026-07-07.

### Other changes

- Analyze multiple feature profiles ([#104](https://github.com/astral-sh/hawk/pull/104))
- Cache diagnostic source files ([#94](https://github.com/astral-sh/hawk/pull/94))
- Encode finding invariants in types ([#101](https://github.com/astral-sh/hawk/pull/101))
- Extract graph analysis library ([#105](https://github.com/astral-sh/hawk/pull/105))
- Harden analysis lifecycle ([#96](https://github.com/astral-sh/hawk/pull/96))
- Harden frontend-driver protocol ([#95](https://github.com/astral-sh/hawk/pull/95))
- Index visibility fix plans ([#92](https://github.com/astral-sh/hawk/pull/92))
- Introduce Hawk test context ([#102](https://github.com/astral-sh/hawk/pull/102))
- Isolate Cargo rustc probe state ([#108](https://github.com/astral-sh/hawk/pull/108))
- Reduce driver collection work ([#98](https://github.com/astral-sh/hawk/pull/98))
- Reject ambiguous library crate names ([#93](https://github.com/astral-sh/hawk/pull/93))
- Reuse production dependency compilations ([#90](https://github.com/astral-sh/hawk/pull/90))
- Scope unstable compiler access ([#103](https://github.com/astral-sh/hawk/pull/103))
- Separate consumer reachability graphs ([#91](https://github.com/astral-sh/hawk/pull/91))
- Support --version ([#89](https://github.com/astral-sh/hawk/pull/89))
- Type diagnostic selectors ([#99](https://github.com/astral-sh/hawk/pull/99))
- Type instrumented Cargo invocations ([#100](https://github.com/astral-sh/hawk/pull/100))
- Unify cfg alternative overrides ([#97](https://github.com/astral-sh/hawk/pull/97))
- Use unique crate names in override fixture ([#107](https://github.com/astral-sh/hawk/pull/107))

## 0.1.4

Released on 2026-07-06.

### Other changes

- Allow Hawk fix passes to dirty the workspace ([#77](https://github.com/astral-sh/hawk/pull/77))
- Diagnose unused restricted items ([#80](https://github.com/astral-sh/hawk/pull/80))
- Disambiguate repeated path module declarations ([#78](https://github.com/astral-sh/hawk/pull/78))
- Document the default target directory ([#84](https://github.com/astral-sh/hawk/pull/84))
- Honor dead code allow attributes ([#81](https://github.com/astral-sh/hawk/pull/81))
- Preserve trait-associated interface visibility ([#75](https://github.com/astral-sh/hawk/pull/75))
- Preserve uniform field visibility ([#73](https://github.com/astral-sh/hawk/pull/73))
- Reject non-UTF-8 arguments without panicking ([#83](https://github.com/astral-sh/hawk/pull/83))
- Support configured compiler aliases ([#82](https://github.com/astral-sh/hawk/pull/82))
- Treat exported symbols as analysis roots ([#79](https://github.com/astral-sh/hawk/pull/79))
- Unify path-module source definitions ([#76](https://github.com/astral-sh/hawk/pull/76))
- Upgrade to Rust 1.96.1 ([#87](https://github.com/astral-sh/hawk/pull/87))

## 0.1.3

Released on 2026-06-02.

- Add restricted visibility lints ([#69](https://github.com/astral-sh/hawk/pull/69))
- Support prebuilt release binaries ([#71](https://github.com/astral-sh/hawk/pull/71))
- Use a portable default target directory ([#70](https://github.com/astral-sh/hawk/pull/70))

## 0.1.2

Released on 2026-05-28.

- Stop publishing prebuilt cargo-hawk binaries ([#65](https://github.com/astral-sh/hawk/pull/65))
- Style Hawk front-end errors ([#67](https://github.com/astral-sh/hawk/pull/67))

## 0.1.1

Released on 2026-05-28.

- Add Rooster release preparation ([#63](https://github.com/astral-sh/hawk/pull/63))
- Add shell installer to cargo-dist releases ([#62](https://github.com/astral-sh/hawk/pull/62))

## 0.1.0

Released on 2026-05-28.

- Add Cargo fix support for visibility findings ([#27](https://github.com/astral-sh/hawk/pull/27))
- Add Hawk lint override configuration ([#17](https://github.com/astral-sh/hawk/pull/17))
- Add Made by Astral footer ([#47](https://github.com/astral-sh/hawk/pull/47))
- Add README motivation for workspace-wide analysis ([#57](https://github.com/astral-sh/hawk/pull/57))
- Add diagnostic enforcement modes ([#23](https://github.com/astral-sh/hawk/pull/23))
- Add dist release pipeline for cargo-hawk ([#58](https://github.com/astral-sh/hawk/pull/58))
- Add dual licensing ([#1](https://github.com/astral-sh/hawk/pull/1))
- Add experimental notice to README ([#56](https://github.com/astral-sh/hawk/pull/56))
- Add explicit compilation target selection ([#20](https://github.com/astral-sh/hawk/pull/20))
- Add scoped diagnostic exclusions ([#54](https://github.com/astral-sh/hawk/pull/54))
- Add target-scoped Hawk overrides ([#21](https://github.com/astral-sh/hawk/pull/21))
- Analyze doctest consumers for visibility requirements ([#44](https://github.com/astral-sh/hawk/pull/44))
- Analyze non-production Cargo targets ([#33](https://github.com/astral-sh/hawk/pull/33))
- Analyze public export paths and modules ([#24](https://github.com/astral-sh/hawk/pull/24))
- Analyze public surface compiled only for tests ([#30](https://github.com/astral-sh/hawk/pull/30))
- Analyze workspace test consumers ([#29](https://github.com/astral-sh/hawk/pull/29))
- Avoid fixes for cfg-alternative declarations ([#40](https://github.com/astral-sh/hawk/pull/40))
- Avoid unsafe fixes for grouped public reexports ([#32](https://github.com/astral-sh/hawk/pull/32))
- Clarify dead enum variant remediation ([#53](https://github.com/astral-sh/hawk/pull/53))
- Configure production binary consumers ([#31](https://github.com/astral-sh/hawk/pull/31))
- Deduplicate diagnostic source rendering ([#19](https://github.com/astral-sh/hawk/pull/19))
- Diagnose associated type aliases ([#11](https://github.com/astral-sh/hawk/pull/11))
- Diagnose dead public unions ([#10](https://github.com/astral-sh/hawk/pull/10))
- Disambiguate same-named override targets ([#52](https://github.com/astral-sh/hawk/pull/52))
- Distinguish product roots with matching crate names ([#42](https://github.com/astral-sh/hawk/pull/42))
- Document Hawk architecture ([#43](https://github.com/astral-sh/hawk/pull/43))
- Document Windows cross-target analysis ([#22](https://github.com/astral-sh/hawk/pull/22))
- Document cargo installation methods ([#55](https://github.com/astral-sh/hawk/pull/55))
- Enable manual cargo-dist releases ([#60](https://github.com/astral-sh/hawk/pull/60))
- Expand analyzed public item coverage ([#25](https://github.com/astral-sh/hawk/pull/25))
- Fix dist release matrix indentation ([#59](https://github.com/astral-sh/hawk/pull/59))
- Fix grouped re-exports across consumer plans ([#45](https://github.com/astral-sh/hawk/pull/45))
- Limit fixes to unnecessary public visibility ([#51](https://github.com/astral-sh/hawk/pull/51))
- Omit unsupported Windows release artifact ([#61](https://github.com/astral-sh/hawk/pull/61))
- Preserve existing graph directory files ([#9](https://github.com/astral-sh/hawk/pull/9))
- Preserve public trait implementation interface types ([#16](https://github.com/astral-sh/hawk/pull/16))
- Preserve visibility required by generated fields ([#26](https://github.com/astral-sh/hawk/pull/26))
- Render rustc-style CLI diagnostics ([#12](https://github.com/astral-sh/hawk/pull/12))
- Report fragment flush failures ([#8](https://github.com/astral-sh/hawk/pull/8))
- Root non-production executables for liveness ([#41](https://github.com/astral-sh/hawk/pull/41))
- Set up continuous integration ([#2](https://github.com/astral-sh/hawk/pull/2))
- Simplify accumulated public-surface analysis ([#7](https://github.com/astral-sh/hawk/pull/7))
- Simplify consumer reachability handling ([#34](https://github.com/astral-sh/hawk/pull/34))
- Streamline Hawk README ([#46](https://github.com/astral-sh/hawk/pull/46))
