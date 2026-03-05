import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
export type CheckoutUpdatePublicCustomFieldData = string | number | boolean | Date;
/**
 * Update an existing checkout session using the client secret.
 */
export type CheckoutUpdatePublic = {
    /**
     * Key-value object storing custom field values.
     */
    customFieldData?: {
        [k: string]: string | number | boolean | Date | null;
    } | null | undefined;
    /**
     * ID of the product to checkout. Must be present in the checkout's product list.
     */
    productId?: string | null | undefined;
    /**
     * ID of the product price to checkout. Must correspond to a price present in the checkout's product list.
     *
     * @deprecated field: This will be removed in a future release, please migrate away from it as soon as possible.
     */
    productPriceId?: string | null | undefined;
    amount?: number | null | undefined;
    customerName?: string | null | undefined;
    customerEmail?: string | null | undefined;
    customerBillingAddress?: Address | null | undefined;
    customerTaxId?: string | null | undefined;
    /**
     * Discount code to apply to the checkout.
     */
    discountCode?: string | null | undefined;
};
/** @internal */
export declare const CheckoutUpdatePublicCustomFieldData$inboundSchema: z.ZodType<CheckoutUpdatePublicCustomFieldData, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutUpdatePublicCustomFieldData$Outbound = string | number | boolean | string;
/** @internal */
export declare const CheckoutUpdatePublicCustomFieldData$outboundSchema: z.ZodType<CheckoutUpdatePublicCustomFieldData$Outbound, z.ZodTypeDef, CheckoutUpdatePublicCustomFieldData>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutUpdatePublicCustomFieldData$ {
    /** @deprecated use `CheckoutUpdatePublicCustomFieldData$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutUpdatePublicCustomFieldData, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutUpdatePublicCustomFieldData$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutUpdatePublicCustomFieldData$Outbound, z.ZodTypeDef, CheckoutUpdatePublicCustomFieldData>;
    /** @deprecated use `CheckoutUpdatePublicCustomFieldData$Outbound` instead. */
    type Outbound = CheckoutUpdatePublicCustomFieldData$Outbound;
}
export declare function checkoutUpdatePublicCustomFieldDataToJSON(checkoutUpdatePublicCustomFieldData: CheckoutUpdatePublicCustomFieldData): string;
export declare function checkoutUpdatePublicCustomFieldDataFromJSON(jsonString: string): SafeParseResult<CheckoutUpdatePublicCustomFieldData, SDKValidationError>;
/** @internal */
export declare const CheckoutUpdatePublic$inboundSchema: z.ZodType<CheckoutUpdatePublic, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutUpdatePublic$Outbound = {
    custom_field_data?: {
        [k: string]: string | number | boolean | string | null;
    } | null | undefined;
    product_id?: string | null | undefined;
    product_price_id?: string | null | undefined;
    amount?: number | null | undefined;
    customer_name?: string | null | undefined;
    customer_email?: string | null | undefined;
    customer_billing_address?: Address$Outbound | null | undefined;
    customer_tax_id?: string | null | undefined;
    discount_code?: string | null | undefined;
};
/** @internal */
export declare const CheckoutUpdatePublic$outboundSchema: z.ZodType<CheckoutUpdatePublic$Outbound, z.ZodTypeDef, CheckoutUpdatePublic>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutUpdatePublic$ {
    /** @deprecated use `CheckoutUpdatePublic$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutUpdatePublic, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutUpdatePublic$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutUpdatePublic$Outbound, z.ZodTypeDef, CheckoutUpdatePublic>;
    /** @deprecated use `CheckoutUpdatePublic$Outbound` instead. */
    type Outbound = CheckoutUpdatePublic$Outbound;
}
export declare function checkoutUpdatePublicToJSON(checkoutUpdatePublic: CheckoutUpdatePublic): string;
export declare function checkoutUpdatePublicFromJSON(jsonString: string): SafeParseResult<CheckoutUpdatePublic, SDKValidationError>;
//# sourceMappingURL=checkoutupdatepublic.d.ts.map