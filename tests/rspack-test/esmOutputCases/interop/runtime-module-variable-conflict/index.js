import * as ns from "./foo.mjs";

function getProto() {
	return "application";
}

it("should avoid conflicts with runtime module variables", () => {
	expect(ns).toBeDefined();
	expect(getProto()).toBe("application");
});

export { getProto };
