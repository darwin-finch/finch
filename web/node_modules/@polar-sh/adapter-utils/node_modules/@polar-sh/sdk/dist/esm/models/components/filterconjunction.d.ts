import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const FilterConjunction: {
    readonly And: "and";
    readonly Or: "or";
};
export type FilterConjunction = ClosedEnum<typeof FilterConjunction>;
/** @internal */
export declare const FilterConjunction$inboundSchema: z.ZodNativeEnum<typeof FilterConjunction>;
/** @internal */
export declare const FilterConjunction$outboundSchema: z.ZodNativeEnum<typeof FilterConjunction>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace FilterConjunction$ {
    /** @deprecated use `FilterConjunction$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly And: "and";
        readonly Or: "or";
    }>;
    /** @deprecated use `FilterConjunction$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly And: "and";
        readonly Or: "or";
    }>;
}
//# sourceMappingURL=filterconjunction.d.ts.map