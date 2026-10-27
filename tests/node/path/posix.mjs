// node:path on POSIX, including the edges of empty, dot and trailing-slash input.
import path from "node:path";
import posix from "node:path/posix";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("same module", path.posix === posix && path === posix.posix);
show("constants", { sep: path.sep, delimiter: path.delimiter });
show("join", [path.join("a", "b", "../c", "./d"), path.join("/a/", "/b"), path.join(""), path.join("a", ""), path.join("..", "a")]);
show("normalize", ["/a//b/../c/.", "a/../../b", "", ".", "./", "a/b/", "//x"].map((p) => path.normalize(p)));
show("resolve", [path.resolve("/a/b", "../c"), path.resolve("/a", "/b", "c"), path.resolve("x") === process.cwd() + "/x", path.resolve() === process.cwd()]);
show("relative", [path.relative("/a/b/c", "/a/d"), path.relative("/a", "/a"), path.relative("/a/b", "/a/b/c/d")]);
show("dirname", ["/a/b/c", "/a/b/", "a", "/", ""].map((p) => path.dirname(p)));
show("basename", [path.basename("/a/b.txt"), path.basename("/a/b.txt", ".txt"), path.basename("/a/b/"), path.basename("b.txt", "b.txt"), path.basename("")]);
show("extname", ["a.txt", "a.b.c", ".bashrc", "a.", "a", "/a.b/c"].map((p) => path.extname(p)));
show("parse", path.parse("/home/u/file.tar.gz"));
show("format", [path.format({ dir: "/x", base: "y.z" }), path.format({ root: "/", name: "n", ext: ".e" }), path.format({ name: "n", ext: "e" })]);
show("isAbsolute", ["/a", "a", "", "./a"].map((p) => path.isAbsolute(p)));
