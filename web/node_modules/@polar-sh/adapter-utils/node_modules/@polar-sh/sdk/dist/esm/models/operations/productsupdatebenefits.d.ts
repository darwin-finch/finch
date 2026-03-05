import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ProductBenefitsUpdate, ProductBenefitsUpdate$Outbound } from "../components/productbenefitsupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type ProductsUpdateBenefitsRequest = {
    id: string;
    productBenefitsUpdate: ProductBenefitsUpdate;
};
/** @internal */
export declare const ProductsUpdateBenefitsRequest$inboundSchema: z.ZodType<ProductsUpdateBenefitsRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type ProductsUpdateBenefitsRequest$Outbound = {
    id: string;
    ProductBenefitsUpdate: ProductBenefitsUpdate$Outbound;
};
/** @internal */
export declare const ProductsUpdateBenefitsRequest$outboundSchema: z.ZodType<ProductsUpdateBenefitsRequest$Outbound, z.ZodTypeDef, ProductsUpdateBenefitsRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ProductsUpdateBenefitsRequest$ {
    /** @deprecated use `ProductsUpdateBenefitsRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ProductsUpdateBenefitsRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `ProductsUpdateBenefitsRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ProductsUpdateBenefitsRequest$Outbound, z.ZodTypeDef, ProductsUpdateBenefitsRequest>;
    /** @deprecated use `ProductsUpdateBenefitsRequest$Outbound` instead. */
    type Outbound = ProductsUpdateBenefitsRequest$Outbound;
}
export declare function productsUpdateBenefitsRequestToJSON(productsUpdateBenefitsRequest: ProductsUpdateBenefitsRequest): string;
export declare function productsUpdateBenefitsRequestFromJSON(jsonString: string): SafeParseResult<ProductsUpdateBenefitsRequest, SDKValidationError>;
//# sourceMappingURL=productsupdatebenefits.d.ts.map