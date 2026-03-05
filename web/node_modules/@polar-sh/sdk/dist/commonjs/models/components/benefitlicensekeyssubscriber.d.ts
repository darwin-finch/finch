import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitLicenseKeysSubscriberProperties, BenefitLicenseKeysSubscriberProperties$Outbound } from "./benefitlicensekeyssubscriberproperties.js";
import { Organization, Organization$Outbound } from "./organization.js";
export type BenefitLicenseKeysSubscriber = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the benefit.
     */
    id: string;
    type?: "license_keys" | undefined;
    /**
     * The description of the benefit.
     */
    description: string;
    /**
     * Whether the benefit is selectable when creating a product.
     */
    selectable: boolean;
    /**
     * Whether the benefit is deletable.
     */
    deletable: boolean;
    /**
     * The ID of the organization owning the benefit.
     */
    organizationId: string;
    organization: Organization;
    properties: BenefitLicenseKeysSubscriberProperties;
};
/** @internal */
export declare const BenefitLicenseKeysSubscriber$inboundSchema: z.ZodType<BenefitLicenseKeysSubscriber, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitLicenseKeysSubscriber$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    type: "license_keys";
    description: string;
    selectable: boolean;
    deletable: boolean;
    organization_id: string;
    organization: Organization$Outbound;
    properties: BenefitLicenseKeysSubscriberProperties$Outbound;
};
/** @internal */
export declare const BenefitLicenseKeysSubscriber$outboundSchema: z.ZodType<BenefitLicenseKeysSubscriber$Outbound, z.ZodTypeDef, BenefitLicenseKeysSubscriber>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitLicenseKeysSubscriber$ {
    /** @deprecated use `BenefitLicenseKeysSubscriber$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitLicenseKeysSubscriber, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitLicenseKeysSubscriber$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitLicenseKeysSubscriber$Outbound, z.ZodTypeDef, BenefitLicenseKeysSubscriber>;
    /** @deprecated use `BenefitLicenseKeysSubscriber$Outbound` instead. */
    type Outbound = BenefitLicenseKeysSubscriber$Outbound;
}
export declare function benefitLicenseKeysSubscriberToJSON(benefitLicenseKeysSubscriber: BenefitLicenseKeysSubscriber): string;
export declare function benefitLicenseKeysSubscriberFromJSON(jsonString: string): SafeParseResult<BenefitLicenseKeysSubscriber, SDKValidationError>;
//# sourceMappingURL=benefitlicensekeyssubscriber.d.ts.map