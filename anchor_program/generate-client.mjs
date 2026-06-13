import { createFromRoot } from "codama";
import { rootNodeFromAnchor } from "@codama/nodes-from-anchor";
import { renderVisitor } from "@codama/renderers-rust";
import { readFileSync } from "fs";

const idl = JSON.parse(
  readFileSync("target/idl/anchor_program.json", "utf8")
);

const codama = createFromRoot(rootNodeFromAnchor(idl));

const outDir = "../crates/examples/pong";

codama.accept(
  renderVisitor(outDir, { deleteFolderBeforeRendering: false })
);

console.log(`Rust client generated at ${outDir}/`);
