import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitBase, BenefitBase$Outbound } from "./benefitbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { ProductMediaFileRead, ProductMediaFileRead$Outbound } from "./productmediafileread.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type CustomerProductPrices = LegacyRecurringProductPrice | ProductPrice;
/**
 * Schema of a product for customer portal.
 */
export type CustomerProduct = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the product.
     */
    id: string;
    /**
     * The name of the product.
     */
    name: string;
    /**
     * The description of the product.
     */
    description: string | null;
    /**
     * The recurring interval of the product. If `None`, the product is a one-time purchase.
     */
    recurringInterval: SubscriptionRecurringInterval | null;
    /**
     * Whether the product is a subscription.
     */
    isRecurring: boolean;
    /**
     * Whether the product is archived and no longer available.
     */
    isArchived: boolean;
    /**
     * The ID of the organization owning the product.
     */
    organizationId: string;
    /**
     * List of available prices for this product.
     */
    prices: Array<LegacyRecurringProductPrice | ProductPrice>;
    /**
     * The benefits granted by the product.
     */
    benefits: Array<BenefitBase>;
    /**
     * The medias associated to the product.
     */
    medias: Array<ProductMediaFileRead>;
};
/** @internal */
export declare const CustomerProductPrices$inboundSchema: z.ZodType<CustomerProductPrices, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerProductPrices$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CustomerProductPrices$outboundSchema: z.ZodType<CustomerProductPrices$Outbound, z.ZodTypeDef, CustomerProductPrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerProductPrices$ {
    /** @deprecated use `CustomerProductPrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerProductPrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerProductPrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerProductPrices$Outbound, z.ZodTypeDef, CustomerProductPrices>;
    /** @deprecated use `CustomerProductPrices$Outbound` instead. */
    type Outbound = CustomerProductPrices$Outbound;
}
export declare function customerProductPricesToJSON(customerProductPrices: CustomerProductPrices): string;
export declare function customerProductPricesFromJSON(jsonString: string): SafeParseResult<CustomerProductPrices, SDKValidationError>;
/** @internal */
export declare const CustomerProduct$inboundSchema: z.ZodType<CustomerProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerProduct$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    name: string;
    description: string | null;
    recurring_interval: string | null;
    is_recurring: boolean;
    is_archived: boolean;
    organization_id: string;
    prices: Array<LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound>;
    benefits: Array<BenefitBase$Outbound>;
    medias: Array<ProductMediaFileRead$Outbound>;
};
/** @internal */
export declare const CustomerProduct$outboundSchema: z.ZodType<CustomerProduct$Outbound, z.ZodTypeDef, CustomerProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerProduct$ {
    /** @deprecated use `CustomerProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerProduct$Outbound, z.ZodTypeDef, CustomerProduct>;
    /** @deprecated use `CustomerProduct$Outbound` instead. */
    type Outbound = CustomerProduct$Outbound;
}
export declare function customerProductToJSON(customerProduct: CustomerProduct): string;
export declare function customerProductFromJSON(jsonString: string): SafeParseResult<CustomerProduct, SDKValidationError>;
//# sourceMappingURL=customerproduct.d.ts.map