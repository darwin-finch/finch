import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const OrderStatus: {
    readonly Pending: "pending";
    readonly Paid: "paid";
    readonly Refunded: "refunded";
    readonly PartiallyRefunded: "partially_refunded";
};
export type OrderStatus = ClosedEnum<typeof OrderStatus>;
/** @internal */
export declare const OrderStatus$inboundSchema: z.ZodNativeEnum<typeof OrderStatus>;
/** @internal */
export declare const OrderStatus$outboundSchema: z.ZodNativeEnum<typeof OrderStatus>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrderStatus$ {
    /** @deprecated use `OrderStatus$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Pending: "pending";
        readonly Paid: "paid";
        readonly Refunded: "refunded";
        readonly PartiallyRefunded: "partially_refunded";
    }>;
    /** @deprecated use `OrderStatus$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Pending: "pending";
        readonly Paid: "paid";
        readonly Refunded: "refunded";
        readonly PartiallyRefunded: "partially_refunded";
    }>;
}
//# sourceMappingURL=orderstatus.d.ts.map