import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
export type CheckoutProductCreateMetadata = string | number | boolean;
export type CheckoutProductCreateCustomFieldData = string | number | boolean | Date;
export type CheckoutProductCreateCustomerMetadata = string | number | boolean;
/**
 * Create a new checkout session from a product.
 *
 * @remarks
 *
 * **Deprecated**: Use `CheckoutProductsCreate` instead.
 *
 * Metadata set on the checkout will be copied
 * to the resulting order and/or subscription.
 */
export type CheckoutProductCreate = {
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
     * ID of the product to checkout. First available price will be selected.
     */
    productId: string;
};
/** @internal */
export declare const CheckoutProductCreateMetadata$inboundSchema: z.ZodType<CheckoutProductCreateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProductCreateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutProductCreateMetadata$outboundSchema: z.ZodType<CheckoutProductCreateMetadata$Outbound, z.ZodTypeDef, CheckoutProductCreateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProductCreateMetadata$ {
    /** @deprecated use `CheckoutProductCreateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProductCreateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProductCreateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProductCreateMetadata$Outbound, z.ZodTypeDef, CheckoutProductCreateMetadata>;
    /** @deprecated use `CheckoutProductCreateMetadata$Outbound` instead. */
    type Outbound = CheckoutProductCreateMetadata$Outbound;
}
export declare function checkoutProductCreateMetadataToJSON(checkoutProductCreateMetadata: CheckoutProductCreateMetadata): string;
export declare function checkoutProductCreateMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutProductCreateMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutProductCreateCustomFieldData$inboundSchema: z.ZodType<CheckoutProductCreateCustomFieldData, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProductCreateCustomFieldData$Outbound = string | number | boolean | string;
/** @internal */
export declare const CheckoutProductCreateCustomFieldData$outboundSchema: z.ZodType<CheckoutProductCreateCustomFieldData$Outbound, z.ZodTypeDef, CheckoutProductCreateCustomFieldData>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProductCreateCustomFieldData$ {
    /** @deprecated use `CheckoutProductCreateCustomFieldData$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProductCreateCustomFieldData, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProductCreateCustomFieldData$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProductCreateCustomFieldData$Outbound, z.ZodTypeDef, CheckoutProductCreateCustomFieldData>;
    /** @deprecated use `CheckoutProductCreateCustomFieldData$Outbound` instead. */
    type Outbound = CheckoutProductCreateCustomFieldData$Outbound;
}
export declare function checkoutProductCreateCustomFieldDataToJSON(checkoutProductCreateCustomFieldData: CheckoutProductCreateCustomFieldData): string;
export declare function checkoutProductCreateCustomFieldDataFromJSON(jsonString: string): SafeParseResult<CheckoutProductCreateCustomFieldData, SDKValidationError>;
/** @internal */
export declare const CheckoutProductCreateCustomerMetadata$inboundSchema: z.ZodType<CheckoutProductCreateCustomerMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProductCreateCustomerMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutProductCreateCustomerMetadata$outboundSchema: z.ZodType<CheckoutProductCreateCustomerMetadata$Outbound, z.ZodTypeDef, CheckoutProductCreateCustomerMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProductCreateCustomerMetadata$ {
    /** @deprecated use `CheckoutProductCreateCustomerMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProductCreateCustomerMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProductCreateCustomerMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProductCreateCustomerMetadata$Outbound, z.ZodTypeDef, CheckoutProductCreateCustomerMetadata>;
    /** @deprecated use `CheckoutProductCreateCustomerMetadata$Outbound` instead. */
    type Outbound = CheckoutProductCreateCustomerMetadata$Outbound;
}
export declare function checkoutProductCreateCustomerMetadataToJSON(checkoutProductCreateCustomerMetadata: CheckoutProductCreateCustomerMetadata): string;
export declare function checkoutProductCreateCustomerMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutProductCreateCustomerMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutProductCreate$inboundSchema: z.ZodType<CheckoutProductCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutProductCreate$Outbound = {
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
    product_id: string;
};
/** @internal */
export declare const CheckoutProductCreate$outboundSchema: z.ZodType<CheckoutProductCreate$Outbound, z.ZodTypeDef, CheckoutProductCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutProductCreate$ {
    /** @deprecated use `CheckoutProductCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutProductCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutProductCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutProductCreate$Outbound, z.ZodTypeDef, CheckoutProductCreate>;
    /** @deprecated use `CheckoutProductCreate$Outbound` instead. */
    type Outbound = CheckoutProductCreate$Outbound;
}
export declare function checkoutProductCreateToJSON(checkoutProductCreate: CheckoutProductCreate): string;
export declare function checkoutProductCreateFromJSON(jsonString: string): SafeParseResult<CheckoutProductCreate, SDKValidationError>;
//# sourceMappingURL=checkoutproductcreate.d.ts.map