import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { ProductPriceCustom, ProductPriceCustom$Outbound } from "./productpricecustom.js";
import { ProductPriceFixed, ProductPriceFixed$Outbound } from "./productpricefixed.js";
import { ProductPriceFree, ProductPriceFree$Outbound } from "./productpricefree.js";
export type ProductPrice = ProductPriceFree | ProductPriceFixed | ProductPriceCustom;
/** @internal */
export declare const ProductPrice$inboundSchema: z.ZodType<ProductPrice, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductPrice$Outbound = ProductPriceFree$Outbound | ProductPriceFixed$Outbound | ProductPriceCustom$Outbound;
/** @internal */
export declare const ProductPrice$outboundSchema: z.ZodType<ProductPrice$Outbound, z.ZodTypeDef, ProductPrice>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductPrice$ {
    /** @deprecated use `ProductPrice$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductPrice, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductPrice$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductPrice$Outbound, z.ZodTypeDef, ProductPrice>;
    /** @deprecated use `ProductPrice$Outbound` instead. */
    type Outbound = ProductPrice$Outbound;
}
export declare function productPriceToJSON(productPrice: ProductPrice): string;
export declare function productPriceFromJSON(jsonString: string): SafeParseResult<ProductPrice, SDKValidationError>;
//# sourceMappingURL=productprice.d.ts.map