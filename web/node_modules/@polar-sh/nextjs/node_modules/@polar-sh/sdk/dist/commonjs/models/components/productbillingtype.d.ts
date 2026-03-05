import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const ProductBillingType: {
    readonly OneTime: "one_time";
    readonly Recurring: "recurring";
};
export type ProductBillingType = ClosedEnum<typeof ProductBillingType>;
/** @internal */
export declare const ProductBillingType$inboundSchema: z.ZodNativeEnum<typeof ProductBillingType>;
/** @internal */
export declare const ProductBillingType$outboundSchema: z.ZodNativeEnum<typeof ProductBillingType>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductBillingType$ {
    /** @deprecated use `ProductBillingType$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly OneTime: "one_time";
        readonly Recurring: "recurring";
    }>;
    /** @deprecated use `ProductBillingType$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly OneTime: "one_time";
        readonly Recurring: "recurring";
    }>;
}
//# sourceMappingURL=productbillingtype.d.ts.map