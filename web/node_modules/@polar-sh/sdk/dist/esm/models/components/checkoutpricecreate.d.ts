import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
export type CheckoutPriceCreateMetadata = string | number | boolean;
export type CheckoutPriceCreateCustomFieldData = string | number | boolean | Date;
export type CheckoutPriceCreateCustomerMetadata = string | number | boolean;
/**
 * Create a new checkout session from a product price.
 *
 * @remarks
 *
 * **Deprecated**: Use `CheckoutProductsCreate` instead.
 *
 * Metadata set on the checkout will be copied
 * to the resulting order and/or subscription.
 */
export type CheckoutPriceCreate = {
    /**
     * Key-value object allowing you to store additional information.
     *
     * @remarks
     *
     * The key must be a string with a maximum length of **40 characters**.
     * The value must be either:
     *
     * * A string with a maximum length of **500 characters**
     * * An integer
     * * A boolean
     *
     * You can store up to **50 key-value pairs**.
     */
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    /**
     * Key-value object storing custom field values.
     */
    customFieldData?: {
        [k: string]: string | number | boolean | Date | null;
    } | undefined;
    /**
     * ID of the discount to apply to the checkout.
     */
    discountId?: string | null | undefined;
    /**
     * Whether to allow the customer to apply discount codes. If you apply a discount through `discount_id`, it'll still be applied, but the customer won't be able to change it.
     */
    allowDiscountCodes?: boolean | undefined;
    amount?: number | null | undefined;
    /**
     * ID of an existing customer in the organization. The customer data will be pre-filled in the checkout form. The resulting order will be linked to this customer.
     */
    customerId?: string | null | undefined;
    /**
     * ID of the customer in your system. If a matching customer exists on Polar, the resulting order will be linked to this customer. Otherwise, a new customer will be created with this external ID set.
     */
    customerExternalId?: string | null | undefined;
    customerName?: string | null | undefined;
    customerEmail?: string | null | undefined;
    customerIpAddress?: string | null | undefined;
    customerBillingAddress?: Address | null | undefined;
    customerTaxId?: string | null | undefined;
    /**
     * Key-value object allowing you to store additional information that'll be copied to the created customer.
     *
     * @remarks
     *
     * The key must be a string with a maximum length of **40 characters**.
     * The value must be either:
     *
     * * A string with a maximum length of **500 characters**
     * * An integer
     * * A boolean
     *
     * You can store up to **50 key-value pairs**.
     */
    customerMetadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    /**
     * ID of a subscription to upgrade. It must be on a free pricing. If checkout is successful, metadata set on this checkout will be copied to the subscription, and existing keys will be overwritten.
     */
    subscriptionId?: string | null | undefined;
    /**
     * URL where the customer will be redirected after a successful payment.You can add the `checkout_id={CHECKOUT_ID}` query parameter to retrieve the checkout session id.
     */
    successUrl?: string | null | undefined;
    /**
     * If you plan to embed the checkout session, set this to the Origin of the embedding page. It'll allow the Polar iframe to communicate with the parent page.
     */
    embedOrigin?: string | null | undefined;
    /**
     * ID of the product price to checkout.
     */
    productPriceId: string;
};
/** @internal */
export declare const CheckoutPriceCreateMetadata$inboundSchema: z.ZodType<CheckoutPriceCreateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPriceCreateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutPriceCreateMetadata$outboundSchema: z.ZodType<CheckoutPriceCreateMetadata$Outbound, z.ZodTypeDef, CheckoutPriceCreateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPriceCreateMetadata$ {
    /** @deprecated use `CheckoutPriceCreateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPriceCreateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPriceCreateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPriceCreateMetadata$Outbound, z.ZodTypeDef, CheckoutPriceCreateMetadata>;
    /** @deprecated use `CheckoutPriceCreateMetadata$Outbound` instead. */
    type Outbound = CheckoutPriceCreateMetadata$Outbound;
}
export declare function checkoutPriceCreateMetadataToJSON(checkoutPriceCreateMetadata: CheckoutPriceCreateMetadata): string;
export declare function checkoutPriceCreateMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutPriceCreateMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutPriceCreateCustomFieldData$inboundSchema: z.ZodType<CheckoutPriceCreateCustomFieldData, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPriceCreateCustomFieldData$Outbound = string | number | boolean | string;
/** @internal */
export declare const CheckoutPriceCreateCustomFieldData$outboundSchema: z.ZodType<CheckoutPriceCreateCustomFieldData$Outbound, z.ZodTypeDef, CheckoutPriceCreateCustomFieldData>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPriceCreateCustomFieldData$ {
    /** @deprecated use `CheckoutPriceCreateCustomFieldData$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPriceCreateCustomFieldData, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPriceCreateCustomFieldData$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPriceCreateCustomFieldData$Outbound, z.ZodTypeDef, CheckoutPriceCreateCustomFieldData>;
    /** @deprecated use `CheckoutPriceCreateCustomFieldData$Outbound` instead. */
    type Outbound = CheckoutPriceCreateCustomFieldData$Outbound;
}
export declare function checkoutPriceCreateCustomFieldDataToJSON(checkoutPriceCreateCustomFieldData: CheckoutPriceCreateCustomFieldData): string;
export declare function checkoutPriceCreateCustomFieldDataFromJSON(jsonString: string): SafeParseResult<CheckoutPriceCreateCustomFieldData, SDKValidationError>;
/** @internal */
export declare const CheckoutPriceCreateCustomerMetadata$inboundSchema: z.ZodType<CheckoutPriceCreateCustomerMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPriceCreateCustomerMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutPriceCreateCustomerMetadata$outboundSchema: z.ZodType<CheckoutPriceCreateCustomerMetadata$Outbound, z.ZodTypeDef, CheckoutPriceCreateCustomerMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPriceCreateCustomerMetadata$ {
    /** @deprecated use `CheckoutPriceCreateCustomerMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPriceCreateCustomerMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPriceCreateCustomerMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPriceCreateCustomerMetadata$Outbound, z.ZodTypeDef, CheckoutPriceCreateCustomerMetadata>;
    /** @deprecated use `CheckoutPriceCreateCustomerMetadata$Outbound` instead. */
    type Outbound = CheckoutPriceCreateCustomerMetadata$Outbound;
}
export declare function checkoutPriceCreateCustomerMetadataToJSON(checkoutPriceCreateCustomerMetadata: CheckoutPriceCreateCustomerMetadata): string;
export declare function checkoutPriceCreateCustomerMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutPriceCreateCustomerMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutPriceCreate$inboundSchema: z.ZodType<CheckoutPriceCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPriceCreate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    custom_field_data?: {
        [k: string]: string | number | boolean | string | null;
    } | undefined;
    discount_id?: string | null | undefined;
    allow_discount_codes: boolean;
    amount?: number | null | undefined;
    customer_id?: string | null | undefined;
    customer_external_id?: string | null | undefined;
    customer_name?: string | null | undefined;
    customer_email?: string | null | undefined;
    customer_ip_address?: string | null | undefined;
    customer_billing_address?: Address$Outbound | null | undefined;
    customer_tax_id?: string | null | undefined;
    customer_metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    subscription_id?: string | null | undefined;
    success_url?: string | null | undefined;
    embed_origin?: string | null | undefined;
    product_price_id: string;
};
/** @internal */
export declare const CheckoutPriceCreate$outboundSchema: z.ZodType<CheckoutPriceCreate$Outbound, z.ZodTypeDef, CheckoutPriceCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPriceCreate$ {
    /** @deprecated use `CheckoutPriceCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPriceCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPriceCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPriceCreate$Outbound, z.ZodTypeDef, CheckoutPriceCreate>;
    /** @deprecated use `CheckoutPriceCreate$Outbound` instead. */
    type Outbound = CheckoutPriceCreate$Outbound;
}
export declare function checkoutPriceCreateToJSON(checkoutPriceCreate: CheckoutPriceCreate): string;
export declare function checkoutPriceCreateFromJSON(jsonString: string): SafeParseResult<CheckoutPriceCreate, SDKValidationError>;
//# sourceMappingURL=checkoutpricecreate.d.ts.map