import * as z from "zod";
export type NotPermittedData = {
    error: "NotPermitted";
    detail: string;
};
export declare class NotPermitted extends Error {
    error: "NotPermitted";
    detail: string;
    /** The original data that was passed to this error instance. */
    data$: NotPermittedData;
    constructor(err: NotPermittedData);
}
/** @internal */
export declare const NotPermitted$inboundSchema: z.ZodType<NotPermitted, z.ZodTypeDef, unknown>;
/** @internal */
export type NotPermitted$Outbound = {
    error: "NotPermitted";
    detail: string;
};
/** @internal */
export declare const NotPermitted$outboundSchema: z.ZodType<NotPermitted$Outbound, z.ZodTypeDef, NotPermitted>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace NotPermitted$ {
    /** @deprecated use `NotPermitted$inboundSchema` instead. */
    const inboundSchema: z.ZodType<NotPermitted, z.ZodTypeDef, unknown>;
    /** @deprecated use `NotPermitted$outboundSchema` instead. */
    const outboundSchema: z.ZodType<NotPermitted$Outbound, z.ZodTypeDef, NotPermitted>;
    /** @deprecated use `NotPermitted$Outbound` instead. */
    type Outbound = NotPermitted$Outbound;
}
//# sourceMappingURL=notpermitted.d.ts.map