import * as z from "zod";
export type RefundedAlreadyData = {
    error: "RefundedAlready";
    detail: string;
};
export declare class RefundedAlready extends Error {
    error: "RefundedAlready";
    detail: string;
    /** The original data that was passed to this error instance. */
    data$: RefundedAlreadyData;
    constructor(err: RefundedAlreadyData);
}
/** @internal */
export declare const RefundedAlready$inboundSchema: z.ZodType<RefundedAlready, z.ZodTypeDef, unknown>;
/** @internal */
export type RefundedAlready$Outbound = {
    error: "RefundedAlready";
    detail: string;
};
/** @internal */
export declare const RefundedAlready$outboundSchema: z.ZodType<RefundedAlready$Outbound, z.ZodTypeDef, RefundedAlready>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundedAlready$ {
    /** @deprecated use `RefundedAlready$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RefundedAlready, z.ZodTypeDef, unknown>;
    /** @deprecated use `RefundedAlready$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RefundedAlready$Outbound, z.ZodTypeDef, RefundedAlready>;
    /** @deprecated use `RefundedAlready$Outbound` instead. */
    type Outbound = RefundedAlready$Outbound;
}
//# sourceMappingURL=refundedalready.d.ts.map