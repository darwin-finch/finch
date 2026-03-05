import { ZodRawShape } from "zod";
import { ResourceDefinition, ResourceTemplateDefinition } from "./resources.js";
import { ToolDefinition } from "./tools.js";
export type Register = {
    tool: <A extends ZodRawShape | undefined>(def: ToolDefinition<A>) => void;
    resource: (def: ResourceDefinition) => void;
    resourceTemplate: (def: ResourceTemplateDefinition) => void;
};
//# sourceMappingURL=extensions.d.ts.map