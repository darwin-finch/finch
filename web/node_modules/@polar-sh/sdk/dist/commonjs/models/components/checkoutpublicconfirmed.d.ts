import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
import { AttachedCustomField, AttachedCustomField$Outbound } from "./attachedcustomfield.js";
import { CheckoutDiscountFixedOnceForeverDuration, CheckoutDiscountFixedOnceForeverDuration$Outbound } from "./checkoutdiscountfixedonceforeverduration.js";
import { CheckoutDiscountFixedRepeatDuration, CheckoutDiscountFixedRepeatDuration$Outbound } from "./checkoutdiscountfixedrepeatduration.js";
import { CheckoutDiscountPercentageOnceForeverDuration, CheckoutDiscountPercentageOnceForeverDuration$Outbound } from "./checkoutdiscountpercentageonceforeverduration.js";
import { CheckoutDiscountPercentageRepeatDuration, CheckoutDiscountPercentageRepeatDuration$Outbound } from "./checkoutdiscountpercentagerepeatduration.js";
import { CheckoutProduct, CheckoutProduct$Outbound } from "./checkoutproduct.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { Organization, Organization$Outbound } from "./organization.js";
import { PaymentProcessor } from "./paymentprocessor.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
export type CheckoutPublicConfirmedCustomFieldData = string | number | boolean | Date;
/**
 * Price of the selected product.
 */
export type CheckoutPublicConfirmedProductPrice = LegacyRecurringProductPrice | ProductPrice;
export type CheckoutPublicConfirmedDiscount = CheckoutDiscountPercentageOnceForeverDuration | CheckoutDiscountFixedOnceForeverDuration | CheckoutDiscountPercentageRepeatDuration | CheckoutDiscountFixedRepeatDuration;
/**
 * Checkout session data retrieved using the client secret after confirmation.
 *
 * @remarks
 *
 * It contains a customer session token to retrieve order information
 * right after the checkout.
 */
export type CheckoutPublicConfirmed = {
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
     * Key-value object storing custom field values.
     */
    customFieldData?: {
        [k: string]: string | number | boolean | Date | null;
    } | undefined;
    paymentProcessor: PaymentProcessor;
    status?: "confirmed" | undefined;
    /**
     * Client secret used to update and complete the checkout session from the client.
     */
    clientSecret: string;
    /**
     * URL where the customer can access the checkout session.
     */
    url: string;
    /**
     * Expiration date and time of the checkout session.
     */
    expiresAt: Date;
    /**
     * URL where the customer will be redirected after a successful payment.
     */
    successUrl: string;
    /**
     * When checkout is embedded, represents the Origin of the page embedding the checkout. Used as a security measure to send messages only to the embedding page.
     */
    embedOrigin: string | null;
    amount: number | null;
    /**
     * Computed tax amount to pay in cents.
     */
    taxAmount: number | null;
    /**
     * Currency code of the checkout session.
     */
    currency: string | null;
    /**
     * Subtotal amount in cents, including discounts and before tax.
     */
    subtotalAmount: number | null;
    /**
     * Total amount to pay in cents, including discounts and after tax.
     */
    totalAmount: number | null;
    /**
     * ID of the product to checkout.
     */
    productId: string;
    /**
     * ID of the product price to checkout.
     */
    productPriceId: string;
    /**
     * ID of the discount applied to the checkout.
     */
    discountId: string | null;
    /**
     * Whether to allow the customer to apply discount codes. If you apply a discount through `discount_id`, it'll still be applied, but the customer won't be able to change it.
     */
    allowDiscountCodes: boolean;
    /**
     * Whether the discount is applicable to the checkout. Typically, free and custom prices are not discountable.
     */
    isDiscountApplicable: boolean;
    /**
     * Whether the product price is free, regardless of discounts.
     */
    isFreeProductPrice: boolean;
    /**
     * Whether the checkout requires payment, e.g. in case of free products or discounts that cover the total amount.
     */
    isPaymentRequired: boolean;
    /**
     * Whether the checkout requires setting up a payment method, regardless of the amount, e.g. subscriptions that have first free cycles.
     */
    isPaymentSetupRequired: boolean;
    /**
     * Whether the checkout requires a payment form, whether because of a payment or payment method setup.
     */
    isPaymentFormRequired: boolean;
    customerId: string | null;
    /**
     * Name of the customer.
     */
    customerName: string | null;
    /**
     * Email address of the customer.
     */
    customerEmail: string | null;
    customerIpAddress: string | null;
    customerBillingAddress: Address | null;
    customerTaxId: string | null;
    paymentProcessorMetadata: {
        [k: string]: string;
    };
    /**
     * List of products available to select.
     */
    products: Array<CheckoutProduct>;
    /**
     * Product data for a checkout session.
     */
    product: CheckoutProduct;
    /**
     * Price of the selected product.
     */
    productPrice: LegacyRecurringProductPrice | ProductPrice;
    discount: CheckoutDiscountPercentageOnceForeverDuration | CheckoutDiscountFixedOnceForeverDuration | CheckoutDiscountPercentageRepeatDuration | CheckoutDiscountFixedRepeatDuration | null;
    organization: Organization;
    attachedCustomFields: Array<AttachedCustomField>;
    customerSessionToken: string;
};
/** @internal */
export declare const CheckoutPublicConfirmedCustomFieldData$inboundSchema: z.ZodType<CheckoutPublicConfirmedCustomFieldData, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPublicConfirmedCustomFieldData$Outbound = string | number | boolean | string;
/** @internal */
export declare const CheckoutPublicConfirmedCustomFieldData$outboundSchema: z.ZodType<CheckoutPublicConfirmedCustomFieldData$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedCustomFieldData>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPublicConfirmedCustomFieldData$ {
    /** @deprecated use `CheckoutPublicConfirmedCustomFieldData$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPublicConfirmedCustomFieldData, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPublicConfirmedCustomFieldData$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPublicConfirmedCustomFieldData$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedCustomFieldData>;
    /** @deprecated use `CheckoutPublicConfirmedCustomFieldData$Outbound` instead. */
    type Outbound = CheckoutPublicConfirmedCustomFieldData$Outbound;
}
export declare function checkoutPublicConfirmedCustomFieldDataToJSON(checkoutPublicConfirmedCustomFieldData: CheckoutPublicConfirmedCustomFieldData): string;
export declare function checkoutPublicConfirmedCustomFieldDataFromJSON(jsonString: string): SafeParseResult<CheckoutPublicConfirmedCustomFieldData, SDKValidationError>;
/** @internal */
export declare const CheckoutPublicConfirmedProductPrice$inboundSchema: z.ZodType<CheckoutPublicConfirmedProductPrice, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPublicConfirmedProductPrice$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CheckoutPublicConfirmedProductPrice$outboundSchema: z.ZodType<CheckoutPublicConfirmedProductPrice$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedProductPrice>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPublicConfirmedProductPrice$ {
    /** @deprecated use `CheckoutPublicConfirmedProductPrice$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPublicConfirmedProductPrice, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPublicConfirmedProductPrice$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPublicConfirmedProductPrice$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedProductPrice>;
    /** @deprecated use `CheckoutPublicConfirmedProductPrice$Outbound` instead. */
    type Outbound = CheckoutPublicConfirmedProductPrice$Outbound;
}
export declare function checkoutPublicConfirmedProductPriceToJSON(checkoutPublicConfirmedProductPrice: CheckoutPublicConfirmedProductPrice): string;
export declare function checkoutPublicConfirmedProductPriceFromJSON(jsonString: string): SafeParseResult<CheckoutPublicConfirmedProductPrice, SDKValidationError>;
/** @internal */
export declare const CheckoutPublicConfirmedDiscount$inboundSchema: z.ZodType<CheckoutPublicConfirmedDiscount, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPublicConfirmedDiscount$Outbound = CheckoutDiscountPercentageOnceForeverDuration$Outbound | CheckoutDiscountFixedOnceForeverDuration$Outbound | CheckoutDiscountPercentageRepeatDuration$Outbound | CheckoutDiscountFixedRepeatDuration$Outbound;
/** @internal */
export declare const CheckoutPublicConfirmedDiscount$outboundSchema: z.ZodType<CheckoutPublicConfirmedDiscount$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedDiscount>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPublicConfirmedDiscount$ {
    /** @deprecated use `CheckoutPublicConfirmedDiscount$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPublicConfirmedDiscount, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPublicConfirmedDiscount$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPublicConfirmedDiscount$Outbound, z.ZodTypeDef, CheckoutPublicConfirmedDiscount>;
    /** @deprecated use `CheckoutPublicConfirmedDiscount$Outbound` instead. */
    type Outbound = CheckoutPublicConfirmedDiscount$Outbound;
}
export declare function checkoutPublicConfirmedDiscountToJSON(checkoutPublicConfirmedDiscount: CheckoutPublicConfirmedDiscount): string;
export declare function checkoutPublicConfirmedDiscountFromJSON(jsonString: string): SafeParseResult<CheckoutPublicConfirmedDiscount, SDKValidationError>;
/** @internal */
export declare const CheckoutPublicConfirmed$inboundSchema: z.ZodType<CheckoutPublicConfirmed, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutPublicConfirmed$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    custom_field_data?: {
        [k: string]: string | number | boolean | string | null;
    } | undefined;
    payment_processor: string;
    status: "confirmed";
    client_secret: string;
    url: string;
    expires_at: string;
    success_url: string;
    embed_origin: string | null;
    amount: number | null;
    tax_amount: number | null;
    currency: string | null;
    subtotal_amount: number | null;
    total_amount: number | null;
    product_id: string;
    product_price_id: string;
    discount_id: string | null;
    allow_discount_codes: boolean;
    is_discount_applicable: boolean;
    is_free_product_price: boolean;
    is_payment_required: boolean;
    is_payment_setup_required: boolean;
    is_payment_form_required: boolean;
    customer_id: string | null;
    customer_name: string | null;
    customer_email: string | null;
    customer_ip_address: string | null;
    customer_billing_address: Address$Outbound | null;
    customer_tax_id: string | null;
    payment_processor_metadata: {
        [k: string]: string;
    };
    products: Array<CheckoutProduct$Outbound>;
    product: CheckoutProduct$Outbound;
    product_price: LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
    discount: CheckoutDiscountPercentageOnceForeverDuration$Outbound | CheckoutDiscountFixedOnceForeverDuration$Outbound | CheckoutDiscountPercentageRepeatDuration$Outbound | CheckoutDiscountFixedRepeatDuration$Outbound | null;
    organization: Organization$Outbound;
    attached_custom_fields: Array<AttachedCustomField$Outbound>;
    customer_session_token: string;
};
/** @internal */
export declare const CheckoutPublicConfirmed$outboundSchema: z.ZodType<CheckoutPublicConfirmed$Outbound, z.ZodTypeDef, CheckoutPublicConfirmed>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutPublicConfirmed$ {
    /** @deprecated use `CheckoutPublicConfirmed$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutPublicConfirmed, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutPublicConfirmed$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutPublicConfirmed$Outbound, z.ZodTypeDef, CheckoutPublicConfirmed>;
    /** @deprecated use `CheckoutPublicConfirmed$Outbound` instead. */
    type Outbound = CheckoutPublicConfirmed$Outbound;
}
export declare function checkoutPublicConfirmedToJSON(checkoutPublicConfirmed: CheckoutPublicConfirmed): string;
export declare function checkoutPublicConfirmedFromJSON(jsonString: string): SafeParseResult<CheckoutPublicConfirmed, SDKValidationError>;
//# sourceMappingURL=checkoutpublicconfirmed.d.ts.map