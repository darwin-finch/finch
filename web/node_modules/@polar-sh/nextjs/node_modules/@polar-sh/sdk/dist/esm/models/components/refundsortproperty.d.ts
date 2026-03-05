import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const RefundSortProperty: {
    readonly CreatedAt: "created_at";
    readonly MinusCreatedAt: "-created_at";
    readonly Amount: "amount";
    readonly MinusAmount: "-amount";
};
export type RefundSortProperty = ClosedEnum<typeof RefundSortProperty>;
/** @internal */
export declare const RefundSortProperty$inboundSchema: z.ZodNativeEnum<typeof RefundSortProperty>;
/** @internal */
export declare const RefundSortProperty$outboundSchema: z.ZodNativeEnum<typeof RefundSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundSortProperty$ {
    /** @deprecated use `RefundSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
    }>;
    /** @deprecated use `RefundSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
    }>;
}
//# sourceMappingURL=refundsortproperty.d.ts.map