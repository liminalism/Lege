# lege-codecs

Canonical source for the three codecs Lege maintains. Each crate keeps its own Cargo graph and its own license files. The root workspace does not list them as members. It patches `jbig2enc-rust` and `jp2lam` onto the processor, and `lege` links `djvu_encoder` from `djvulibrust` directly.

Published crates are the right dependency for other Rust projects. A moving git dependency on this tree is not.

| Directory | Crate | Role |
|---|---|---|
| [jbig2enc-rust](jbig2enc-rust/README.md) | `jbig2enc-rust` | JBIG2 encode and decode. Generic, symbol, and symbol-unify modes. |
| [jp2lam](jp2lam/README.md) | `jp2lam` | JPEG 2000 Part 1 encode, and JP2 decode for document images. |
| [djvulibrust](djvulibrust/README.md) | `djvu_encoder` | DjVu encode: JB2, IW44, hidden text, multi-page DJVM. |

```sh
cargo jbig2
cargo jp2
cargo djvu
```

`cargo djvu` builds the diagnostic `djvu-encoder` binary (`--features cli`). The processor does not shell out to that binary.

Former standalone GitHub repositories for these codecs are archive pages. Issues and development belong in [github.com/liminalism/lege](https://github.com/liminalism/lege).
