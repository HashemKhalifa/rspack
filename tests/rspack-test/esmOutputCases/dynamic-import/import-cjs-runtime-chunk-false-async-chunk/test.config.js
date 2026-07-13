const fs = require("fs");
const path = require("path");

module.exports = {
	afterExecute(options) {
		const readAsset = file =>
			fs.readFileSync(path.join(options.output.path, file), "utf-8");

		const entry = readAsset("main.mjs");
		const dynamic = readAsset("dynamic.mjs");
		const runtime = readAsset("runtime.mjs");

		expect(fs.existsSync(path.join(options.output.path, "index_js.mjs"))).toBe(
			false,
		);
		if (globalThis.__RSPACK_TEST_RUNTIME_MODE_RSPACK) {
			expect(entry).toContain(
				'import { __rspack_require, __rspack_create_fake_namespace_object } from "./runtime.mjs";'
			);
			expect(entry).toContain(
				'__rspack_create_fake_namespace_object(module.createRequire(import.meta.url)("node:stream"), 22)'
			);
			expect(runtime).toContain("export { __rspack_require");
			expect(runtime).toContain("__rspack_create_fake_namespace_object");
			expect(runtime.match(/var __rspack_module_factories/g)).toHaveLength(1);
			expect(entry).not.toContain("export { __rspack_require");
			expect(entry).not.toContain("as __rspack_require");
			expect(dynamic).toContain(
				'import { __rspack_require, __rspack_module_factories, __rspack_compat_get_default_export } from "./runtime.mjs";'
			);
		} else {
			expect(entry).toContain(
				'import { __webpack_require__ } from "./runtime.mjs";'
			);
			expect(entry).toContain(
				'__webpack_require__.t(module.createRequire(import.meta.url)("node:stream"), 22)'
			);
			expect(runtime).toContain("export { __webpack_require__");
			expect(entry).not.toContain("export { __webpack_require__");
			expect(entry).not.toContain("as __webpack_require__");
			expect(dynamic).toContain(
				'import { __webpack_require__ } from "./runtime.mjs";'
			);
		}
		expect(entry).toContain("Promise.all");
		expect(entry).toContain('import("./dynamic.mjs")');
		expect(dynamic).not.toContain('from "./main.mjs"');
	},
};
