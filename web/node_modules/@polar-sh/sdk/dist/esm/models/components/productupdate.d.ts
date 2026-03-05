import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { AttachedCustomFieldCreate, AttachedCustomFieldCreate$Outbound } from "./attachedcustomfieldcreate.js";
import { ExistingProductPrice, ExistingProductPrice$Outbound } from "./existingproductprice.js";
import { ProductPriceCustomCreate, ProductPriceCustomCreate$Outbound } from "./productpricecustomcreate.js";
import { ProductPriceFixedCreate, ProductPriceFixedCreate$Outbound } from "./productpricefixedcreate.js";
import { ProductPriceFreeCreate, ProductPriceFreeCreate$Outbound } from "./productpricefreecreate.js";
import { SubscriptionRecurringInterval } from "./subscriptionrecurringinterval.js";
export type ProductUpdateMetadata = string | number | boolean;
export type ProductUpdatePrices = ExistingProductPrice | ProductPriceFreeCreate | ProductPriceFixedCreate | ProductPriceCustomCreate;
/**
 * Schema to update a product.
 */
export type ProductUpdate = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    name?: string | null | undefined;
    /**
     * The description of the product.
     */
    description?: string | null | undefined;
    /**
     * The recurring interval of the product. If `None`, the product is a one-time purchase. **Can only be set on legacy recurring products. Once set, it can't be changed.**
     */
    recurringInterval?: SubscriptionRecurringInterval | null | undefined;
    /**
     * Whether the product is archived. If `true`, the product won't be available for purchase anymore. Existing customers will still have access to their benefits, and subscriptions will continue normally.
     */
    isArchived?: boolean | null | undefined;
    /**
     * List of available prices for this product. If you want to keep existing prices, include them in the list as an `ExistingProductPrice` object.
     */
    prices?: Array<ExistingProductPrice | ProductPriceFreeCreate | ProductPriceFixedCreate | ProductPriceCustomCreate> | null | undefined;
    /**
     * List of file IDs. Each one must be on the same organization as the product, of type `product_media` and correctly uploaded.
     */
    medias?: Array<string> | null | undefined;
    attachedCustomFields?: Array<AttachedCustomFieldCreate> | null | undefined;
};
/** @internal */
export declare const ProductUpdateMetadata$inboundSchema: z.ZodType<ProductUpdateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductUpdateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const ProductUpdateMetadata$outboundSchema: z.ZodType<ProductUpdateMetadata$Outbound, z.ZodTypeDef, ProductUpdateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductUpdateMetadata$ {
    /** @deprecated use `ProductUpdateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductUpdateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductUpdateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductUpdateMetadata$Outbound, z.ZodTypeDef, ProductUpdateMetadata>;
    /** @deprecated use `ProductUpdateMetadata$Outbound` instead. */
    type Outbound = ProductUpdateMetadata$Outbound;
}
export declare function productUpdateMetadataToJSON(productUpdateMetadata: ProductUpdateMetadata): string;
export declare function productUpdateMetadataFromJSON(jsonString: string): SafeParseResult<ProductUpdateMetadata, SDKValidationError>;
/** @internal */
export declare const ProductUpdatePrices$inboundSchema: z.ZodType<ProductUpdatePrices, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductUpdatePrices$Outbound = ExistingProductPrice$Outbound | ProductPriceFreeCreate$Outbound | ProductPriceFixedCreate$Outbound | ProductPriceCustomCreate$Outbound;
/** @internal */
export declare const ProductUpdatePrices$outboundSchema: z.ZodType<ProductUpdatePrices$Outbound, z.ZodTypeDef, ProductUpdatePrices>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductUpdatePrices$ {
    /** @deprecated use `ProductUpdatePrices$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductUpdatePrices, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductUpdatePrices$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductUpdatePrices$Outbound, z.ZodTypeDef, ProductUpdatePrices>;
    /** @deprecated use `ProductUpdatePrices$Outbound` instead. */
    type Outbound = ProductUpdatePrices$Outbound;
}
export declare function productUpdatePricesToJSON(productUpdatePrices: ProductUpdatePrices): string;
export declare function productUpdatePricesFromJSON(jsonString: string): SafeParseResult<ProductUpdatePrices, SDKValidationError>;
/** @internal */
export declare const ProductUpdate$inboundSchema: z.ZodType<ProductUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductUpdate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    name?: string | null | undefined;
    description?: string | null | undefined;
    recurring_interval?: string | null | undefined;
    is_archived?: boolean | null | undefined;
    prices?: Array<ExistingProductPrice$Outbound | ProductPriceFreeCreate$Outbound | ProductPriceFixedCreate$Outbound | ProductPriceCustomCreate$Outbound> | null | undefined;
    medias?: Array<string> | null | undefined;
    attached_custom_fields?: Array<AttachedCustomFieldCreate$Outbound> | null | undefined;
};
/** @internal */
export declare const ProductUpdate$outboundSchema: z.ZodType<ProductUpdate$Outbound, z.ZodTypeDef, ProductUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductUpdate$ {
    /** @deprecated use `ProductUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductUpdate$Outbound, z.ZodTypeDef, ProductUpdate>;
    /** @deprecated use `ProductUpdate$Outbound` instead. */
    type Outbound = ProductUpdate$Outbound;
}
export declare function productUpdateToJSON(productUpdate: ProductUpdate): string;
export declare function productUpdateFromJSON(jsonString: string): SafeParseResult<ProductUpdate, SDKValidationError>;
//# sourceMappingURL=productupdate.d.ts.map