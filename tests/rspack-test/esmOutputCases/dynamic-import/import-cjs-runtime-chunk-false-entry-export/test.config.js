const fs = require("fs");
const path = require("path");

module.exports = {
	afterExecute(options) {
		if (!globalThis.__RSPACK_TEST_RUNTIME_MODE_RSPACK) {
			return;
		}

		const readAsset = file =>
			fs.readFileSync(path.join(options.output.path, file), "utf-8");

		const entry = readAsset("index_js.mjs");
		const dynamic = readAsset("dynamic.mjs");

		expect(dynamic).not.toContain("__rspack_context");
		expect(dynamic).toContain("__rspack_module_factories.add");
		expect(dynamic).toContain('__rspack_require(/*! ./shared */ "./shared.js")');
		expect(dynamic).toContain("__rspack_compat_get_default_export(dynamic)");

		expect(entry).not.toContain("export { __rspack_context");
		expect(entry).toContain("var __rspack_modules = {};");
		expect(entry).toContain("function __rspack_require(moduleId)");
		expect(entry).toContain(
			'__rspack_create_fake_namespace_object.bind(__rspack_require, /*! ./dynamic */ "./dynamic.js", 19)'
		);
	}
};
