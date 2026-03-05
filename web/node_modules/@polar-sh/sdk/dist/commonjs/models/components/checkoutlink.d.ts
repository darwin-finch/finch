import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CheckoutLinkProduct, CheckoutLinkProduct$Outbound } from "./checkoutlinkproduct.js";
import { DiscountFixedOnceForeverDurationBase, DiscountFixedOnceForeverDurationBase$Outbound } from "./discountfixedonceforeverdurationbase.js";
import { DiscountFixedRepeatDurationBase, DiscountFixedRepeatDurationBase$Outbound } from "./discountfixedrepeatdurationbase.js";
import { DiscountPercentageOnceForeverDurationBase, DiscountPercentageOnceForeverDurationBase$Outbound } from "./discountpercentageonceforeverdurationbase.js";
import { DiscountPercentageRepeatDurationBase, DiscountPercentageRepeatDurationBase$Outbound } from "./discountpercentagerepeatdurationbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { PaymentProcessor } from "./paymentprocessor.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
export type CheckoutLinkMetadata = string | number | boolean;
export type CheckoutLinkDiscount = DiscountPercentageOnceForeverDurationBase | DiscountFixedOnceForeverDurationBase | DiscountPercentageRepeatDurationBase | DiscountFixedRepeatDurationBase;
/**
 * @deprecated class: This will be removed in a future release, please migrate away from it as soon as possible.
 */
export type CheckoutLinkProductPrice = LegacyRecurringProductPrice | ProductPrice;
/**
 * Checkout link data.
 */
export type CheckoutLink = {
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
    metadata: {
        [k: string]: string | number | boolean;
    };
    paymentProcessor: PaymentProcessor;
    /**
     * Client secret used to access the checkout link.
     */
    clientSecret: string;
    /**
     * URL where the customer will be redirected after a successful payment.
     */
    successUrl: string | null;
    /**
     * Optional label to distinguish links internally
     */
    label: string | null;
    /**
     * Whether to allow the customer to apply discount codes. If you apply a discount through `discount_id`, it'll still be applied, but the customer won't be able to change it.
     */
    allowDiscountCodes: boolean;
    /**
     * ID of the discount to apply to the checkout. If the discount is not applicable anymore when opening the checkout link, it'll be ignored.
     */
    discountId: string | null;
    /**
     * The organization ID.
     */
    organizationId: string;
    products: Array<CheckoutLinkProduct>;
    discount: DiscountPercentageOnceForeverDurationBase | DiscountFixedOnceForeverDurationBase | DiscountPercentageRepeatDurationBase | DiscountFixedRepeatDurationBase | null;
    /**
     * @deprecated field: This will be removed in a future release, please migrate away from it as soon as possible.
     */
    productId: string;
    /**
     * @deprecated field: This will be removed in a future release, please migrate away from it as soon as possible.
     */
    productPriceId: string;
    /**
     * Product data for a checkout link.
     */
    product: CheckoutLinkProduct;
    /**
     * @deprecated field: This will be removed in a future release, please migrate away from it as soon as possible.
     */
    productPrice: LegacyRecurringProductPrice | ProductPrice;
    url: string;
};
/** @internal */
export declare const CheckoutLinkMetadata$inboundSchema: z.ZodType<CheckoutLinkMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CheckoutLinkMetadata$outboundSchema: z.ZodType<CheckoutLinkMetadata$Outbound, z.ZodTypeDef, CheckoutLinkMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkMetadata$ {
    /** @deprecated use `CheckoutLinkMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkMetadata$Outbound, z.ZodTypeDef, CheckoutLinkMetadata>;
    /** @deprecated use `CheckoutLinkMetadata$Outbound` instead. */
    type Outbound = CheckoutLinkMetadata$Outbound;
}
export declare function checkoutLinkMetadataToJSON(checkoutLinkMetadata: CheckoutLinkMetadata): string;
export declare function checkoutLinkMetadataFromJSON(jsonString: string): SafeParseResult<CheckoutLinkMetadata, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkDiscount$inboundSchema: z.ZodType<CheckoutLinkDiscount, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkDiscount$Outbound = DiscountPercentageOnceForeverDurationBase$Outbound | DiscountFixedOnceForeverDurationBase$Outbound | DiscountPercentageRepeatDurationBase$Outbound | DiscountFixedRepeatDurationBase$Outbound;
/** @internal */
export declare const CheckoutLinkDiscount$outboundSchema: z.ZodType<CheckoutLinkDiscount$Outbound, z.ZodTypeDef, CheckoutLinkDiscount>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkDiscount$ {
    /** @deprecated use `CheckoutLinkDiscount$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkDiscount, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkDiscount$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkDiscount$Outbound, z.ZodTypeDef, CheckoutLinkDiscount>;
    /** @deprecated use `CheckoutLinkDiscount$Outbound` instead. */
    type Outbound = CheckoutLinkDiscount$Outbound;
}
export declare function checkoutLinkDiscountToJSON(checkoutLinkDiscount: CheckoutLinkDiscount): string;
export declare function checkoutLinkDiscountFromJSON(jsonString: string): SafeParseResult<CheckoutLinkDiscount, SDKValidationError>;
/** @internal */
export declare const CheckoutLinkProductPrice$inboundSchema: z.ZodType<CheckoutLinkProductPrice, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLinkProductPrice$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CheckoutLinkProductPrice$outboundSchema: z.ZodType<CheckoutLinkProductPrice$Outbound, z.ZodTypeDef, CheckoutLinkProductPrice>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLinkProductPrice$ {
    /** @deprecated use `CheckoutLinkProductPrice$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLinkProductPrice, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLinkProductPrice$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLinkProductPrice$Outbound, z.ZodTypeDef, CheckoutLinkProductPrice>;
    /** @deprecated use `CheckoutLinkProductPrice$Outbound` instead. */
    type Outbound = CheckoutLinkProductPrice$Outbound;
}
export declare function checkoutLinkProductPriceToJSON(checkoutLinkProductPrice: CheckoutLinkProductPrice): string;
export declare function checkoutLinkProductPriceFromJSON(jsonString: string): SafeParseResult<CheckoutLinkProductPrice, SDKValidationError>;
/** @internal */
export declare const CheckoutLink$inboundSchema: z.ZodType<CheckoutLink, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutLink$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    metadata: {
        [k: string]: string | number | boolean;
    };
    payment_processor: string;
    client_secret: string;
    success_url: string | null;
    label: string | null;
    allow_discount_codes: boolean;
    discount_id: string | null;
    organization_id: string;
    products: Array<CheckoutLinkProduct$Outbound>;
    discount: DiscountPercentageOnceForeverDurationBase$Outbound | DiscountFixedOnceForeverDurationBase$Outbound | DiscountPercentageRepeatDurationBase$Outbound | DiscountFixedRepeatDurationBase$Outbound | null;
    product_id: string;
    product_price_id: string;
    product: CheckoutLinkProduct$Outbound;
    product_price: LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
    url: string;
};
/** @internal */
export declare const CheckoutLink$outboundSchema: z.ZodType<CheckoutLink$Outbound, z.ZodTypeDef, CheckoutLink>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutLink$ {
    /** @deprecated use `CheckoutLink$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutLink, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutLink$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutLink$Outbound, z.ZodTypeDef, CheckoutLink>;
    /** @deprecated use `CheckoutLink$Outbound` instead. */
    type Outbound = CheckoutLink$Outbound;
}
export declare function checkoutLinkToJSON(checkoutLink: CheckoutLink): string;
export declare function checkoutLinkFromJSON(jsonString: string): SafeParseResult<CheckoutLink, SDKValidationError>;
//# sourceMappingURL=checkoutlink.d.ts.map