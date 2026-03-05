import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const FilterOperator: {
    readonly Eq: "eq";
    readonly Ne: "ne";
    readonly Gt: "gt";
    readonly Gte: "gte";
    readonly Lt: "lt";
    readonly Lte: "lte";
    readonly Like: "like";
    readonly NotLike: "not_like";
};
export type FilterOperator = ClosedEnum<typeof FilterOperator>;
/** @internal */
export declare const FilterOperator$inboundSchema: z.ZodNativeEnum<typeof FilterOperator>;
/** @internal */
export declare const FilterOperator$outboundSchema: z.ZodNativeEnum<typeof FilterOperator>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace FilterOperator$ {
    /** @deprecated use `FilterOperator$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Eq: "eq";
        readonly Ne: "ne";
        readonly Gt: "gt";
        readonly Gte: "gte";
        readonly Lt: "lt";
        readonly Lte: "lte";
        readonly Like: "like";
        readonly NotLike: "not_like";
    }>;
    /** @deprecated use `FilterOperator$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Eq: "eq";
        readonly Ne: "ne";
        readonly Gt: "gt";
        readonly Gte: "gte";
        readonly Lt: "lt";
        readonly Lte: "lte";
        readonly Like: "like";
        readonly NotLike: "not_like";
    }>;
}
//# sourceMappingURL=filteroperator.d.ts.map