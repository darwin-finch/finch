import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitBase, BenefitBase$Outbound } from "./benefitbase.js";
import { LegacyRecurringProductPrice, LegacyRecurringProductPrice$Outbound } from "./legacyrecurringproductprice.js";
import { Organization, Organization$Outbound } from "./organization.js";
import { ProductMediaFileRead, ProductMediaFileRead$Outbound } from "./productmediafileread.js";
import { ProductPrice, ProductPrice$Outbound } from "./productprice.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type CustomerSubscriptionProductPrices = LegacyRecurringProductPrice | ProductPrice;
export type CustomerSubscriptionProduct = {
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
     * List of prices for this product.
     */
    prices: Array<LegacyRecurringProductPrice | ProductPrice>;
    /**
     * List of benefits granted by the product.
     */
    benefits: Array<BenefitBase>;
    /**
     * List of medias associated to the product.
     */
    medias: Array<ProductMediaFileRead>;
    organization: Organization;
};
/** @internal */
export declare const CustomerSubscriptionProductPrices$inboundSchema: z.ZodType<CustomerSubscriptionProductPrices, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSubscriptionProductPrices$Outbound = LegacyRecurringProductPrice$Outbound | ProductPrice$Outbound;
/** @internal */
export declare const CustomerSubscriptionProductPrices$outboundSchema: z.ZodType<CustomerSubscriptionProductPrices$Outbound, z.ZodTypeDef, CustomerSubscriptionProductPrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionProductPrices$ {
    /** @deprecated use `CustomerSubscriptionProductPrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSubscriptionProductPrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSubscriptionProductPrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSubscriptionProductPrices$Outbound, z.ZodTypeDef, CustomerSubscriptionProductPrices>;
    /** @deprecated use `CustomerSubscriptionProductPrices$Outbound` instead. */
    type Outbound = CustomerSubscriptionProductPrices$Outbound;
}
export declare function customerSubscriptionProductPricesToJSON(customerSubscriptionProductPrices: CustomerSubscriptionProductPrices): string;
export declare function customerSubscriptionProductPricesFromJSON(jsonString: string): SafeParseResult<CustomerSubscriptionProductPrices, SDKValidationError>;
/** @internal */
export declare const CustomerSubscriptionProduct$inboundSchema: z.ZodType<CustomerSubscriptionProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSubscriptionProduct$Outbound = {
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
    organization: Organization$Outbound;
};
/** @internal */
export declare const CustomerSubscriptionProduct$outboundSchema: z.ZodType<CustomerSubscriptionProduct$Outbound, z.ZodTypeDef, CustomerSubscriptionProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionProduct$ {
    /** @deprecated use `CustomerSubscriptionProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSubscriptionProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSubscriptionProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSubscriptionProduct$Outbound, z.ZodTypeDef, CustomerSubscriptionProduct>;
    /** @deprecated use `CustomerSubscriptionProduct$Outbound` instead. */
    type Outbound = CustomerSubscriptionProduct$Outbound;
}
export declare function customerSubscriptionProductToJSON(customerSubscriptionProduct: CustomerSubscriptionProduct): string;
export declare function customerSubscriptionProductFromJSON(jsonString: string): SafeParseResult<CustomerSubscriptionProduct, SDKValidationError>;
//# sourceMappingURL=customersubscriptionproduct.d.ts.map