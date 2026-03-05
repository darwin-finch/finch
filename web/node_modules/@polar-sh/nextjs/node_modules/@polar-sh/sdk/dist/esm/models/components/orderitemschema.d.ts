import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * An order line item.
 */
export type OrderItemSchema = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the object.
     */
    id: string;
    /**
     * Description of the line item charge.
     */
    label: string;
    /**
     * Amount in cents, before discounts and taxes.
     */
    amount: number;
    /**
     * Sales tax amount in cents.
     */
    taxAmount: number;
    /**
     * Whether this charge is due to a proration.
     */
    proration: boolean;
    /**
     * Associated price ID, if any.
     */
    productPriceId: string | null;
};
/** @internal */
export declare const OrderItemSchema$inboundSchema: z.ZodType<OrderItemSchema, z.ZodTypeDef, unknown>;
/** @internal */
export type OrderItemSchema$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    label: string;
    amount: number;
    tax_amount: number;
    proration: boolean;
    product_price_id: string | null;
};
/** @internal */
export declare const OrderItemSchema$outboundSchema: z.ZodType<OrderItemSchema$Outbound, z.ZodTypeDef, OrderItemSchema>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrderItemSchema$ {
    /** @deprecated use `OrderItemSchema$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrderItemSchema, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrderItemSchema$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrderItemSchema$Outbound, z.ZodTypeDef, OrderItemSchema>;
    /** @deprecated use `OrderItemSchema$Outbound` instead. */
    type Outbound = OrderItemSchema$Outbound;
}
export declare function orderItemSchemaToJSON(orderItemSchema: OrderItemSchema): string;
export declare function orderItemSchemaFromJSON(jsonString: string): SafeParseResult<OrderItemSchema, SDKValidationError>;
//# sourceMappingURL=orderitemschema.d.ts.map