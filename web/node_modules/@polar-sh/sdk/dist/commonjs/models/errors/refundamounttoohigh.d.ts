import * as z from "zod";
export type RefundAmountTooHighData = {
    error: "RefundAmountTooHigh";
    detail: string;
};
export declare class RefundAmountTooHigh extends Error {
    error: "RefundAmountTooHigh";
    detail: string;
    /** The original data that was passed to this error instance. */
    data$: RefundAmountTooHighData;
    constructor(err: RefundAmountTooHighData);
}
/** @internal */
export declare const RefundAmountTooHigh$inboundSchema: z.ZodType<RefundAmountTooHigh, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundAmountTooHigh$Outbound = {
    error: "RefundAmountTooHigh";
    detail: string;
};
/** @internal */
export declare const RefundAmountTooHigh$outboundSchema: z.ZodType<RefundAmountTooHigh$Outbound, z.ZodTypeDef, RefundAmountTooHigh>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundAmountTooHigh$ {
    /** @deprecated use `RefundAmountTooHigh$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundAmountTooHigh, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundAmountTooHigh$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundAmountTooHigh$Outbound, z.ZodTypeDef, RefundAmountTooHigh>;
    /** @deprecated use `RefundAmountTooHigh$Outbound` instead. */
    type Outbound = RefundAmountTooHigh$Outbound;
}
//# sourceMappingURL=refundamounttoohigh.d.ts.map