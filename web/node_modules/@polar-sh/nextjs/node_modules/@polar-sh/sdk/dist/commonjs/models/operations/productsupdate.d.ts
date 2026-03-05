import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ProductUpdate, ProductUpdate$Outbound } from "../components/productupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type ProductsUpdateRequest = {
    id: string;
    productUpdate: ProductUpdate;
};
/** @internal */
export declare const ProductsUpdateRequest$inboundSchema: z.ZodType<ProductsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductsUpdateRequest$Outbound = {
    id: string;
    ProductUpdate: ProductUpdate$Outbound;
};
/** @internal */
export declare const ProductsUpdateRequest$outboundSchema: z.ZodType<ProductsUpdateRequest$Outbound, z.ZodTypeDef, ProductsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductsUpdateRequest$ {
    /** @deprecated use `ProductsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductsUpdateRequest$Outbound, z.ZodTypeDef, ProductsUpdateRequest>;
    /** @deprecated use `ProductsUpdateRequest$Outbound` instead. */
    type Outbound = ProductsUpdateRequest$Outbound;
}
export declare function productsUpdateRequestToJSON(productsUpdateRequest: ProductsUpdateRequest): string;
export declare function productsUpdateRequestFromJSON(jsonString: string): SafeParseResult<ProductsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=productsupdate.d.ts.map